use lazy_static::lazy_static;
use lsp_types::{CompletionItemKind, Range, SymbolKind, Url};
use regex::Regex;
use tree_sitter::Node;

use crate::utils::*;

use crate::Cli;

struct BuiltinFlags {}
impl BuiltinFlags {
    const IS_OPREATOR: u16 = 1;
    const IGNORE_PARAM_NAME: u16 = 1 << 1;
}

#[derive(Clone, Debug)]
pub(crate) struct Param {
    pub name: String,
    pub default: Option<String>,
    pub range: Range,
}

impl Param {
    pub(crate) fn parse_declaration(code: &str, node: &Node) -> Vec<Param> {
        node.children(&mut node.walk())
            .filter_map(|child| match child.kind() {
                "identifier" => Some(Param {
                    name: node_text(code, &child).to_owned(),
                    default: None,
                    range: child.lsp_range(),
                }),
                "assignment" => child.child_by_field_name("left").and_then(|left| {
                    child.child_by_field_name("right").map(|right| Param {
                        name: node_text(code, &left).to_owned(),
                        default: Some(node_text(code, &right).to_owned()),
                        range: right.lsp_range(),
                    })
                }),
                "special_variable" => None,
                _ => None,
            })
            .collect()
    }

    pub(crate) fn make_snippet(params: &[Param], ignore_name: bool, args: &Cli) -> String {
        params
            .iter()
            .filter(|p| p.default.is_none() || !args.ignore_default)
            .enumerate()
            .map(|(i, p)| {
                if !args.ignore_default && p.default.as_ref().is_some() {
                    return format!("{} = {}", p.name, p.default.as_ref().unwrap());
                }

                if ignore_name {
                    format!("${{{}:{}}}", i + 1, p.name)
                } else {
                    format!("{} = ${{{}:{}}}", p.name, i + 1, p.name)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Default)]
pub(crate) enum ItemKind {
    #[default]
    Variable,
    Function {
        flags: u16,
        params: Vec<Param>,
    },
    Keyword(String),
    Module {
        flags: u16,
        params: Vec<Param>,
    },
}

impl ItemKind {
    pub(crate) fn completion_kind(&self) -> CompletionItemKind {
        match self {
            ItemKind::Variable => CompletionItemKind::VARIABLE,
            ItemKind::Function { .. } => CompletionItemKind::FUNCTION,
            ItemKind::Keyword(_) => CompletionItemKind::KEYWORD,
            ItemKind::Module { .. } => CompletionItemKind::MODULE,
        }
    }
}

#[derive(Default)]
pub(crate) struct Item {
    pub name: String,
    pub kind: ItemKind,
    pub range: Range,
    pub url: Option<Url>,
    pub is_builtin: bool,

    pub(crate) doc: Option<String>,
    pub(crate) hover: Option<String>,
    pub(crate) label: Option<String>,
    pub(crate) snippet: Option<String>,
}

impl Item {
    pub(crate) fn get_snippet(&mut self, args: &Cli) -> String {
        if self.snippet.is_none() {
            self.snippet = Some(self.make_snippet(args));
        }
        self.snippet.as_ref().unwrap().to_owned()
    }

    pub(crate) fn get_hover(&mut self) -> String {
        if self.hover.is_none() {
            self.hover = Some(self.make_hover());
        }
        self.hover.as_ref().unwrap().to_owned()
    }

    pub(crate) fn get_label(&mut self) -> String {
        if self.label.is_none() {
            self.label = Some(self.make_label());
        }
        self.label.as_ref().unwrap().to_owned()
    }

    pub(crate) fn make_snippet(&mut self, args: &Cli) -> String {
        let snippet = match &self.kind {
            ItemKind::Variable => self.name.clone(),
            ItemKind::Function { flags, params } => {
                format!(
                    "{}({});$0",
                    self.name,
                    Param::make_snippet(params, BuiltinFlags::IGNORE_PARAM_NAME & flags != 0, args)
                )
            }
            ItemKind::Keyword(comp) => comp.clone(),
            ItemKind::Module { params, flags } => {
                let params =
                    Param::make_snippet(params, BuiltinFlags::IGNORE_PARAM_NAME & flags != 0, args);
                if BuiltinFlags::IS_OPREATOR & flags != 0 {
                    format!("{}({}) $0", self.name, params)
                } else {
                    format!("{}({});$0", self.name, params)
                }
            }
        };
        self.snippet = Some(snippet.to_owned());
        snippet
    }

    pub(crate) fn make_hover(&self) -> String {
        let mut label = match &self.label {
            Some(label) => label.to_owned(),
            None => self.make_label(),
        };
        label = match self.kind {
            ItemKind::Function { .. } => format!("```scad\nfunction {}\n```", label),
            ItemKind::Module { .. } => format!("```scad\nmodule {}\n```", label),
            _ => format!("```scad\n{}\n```", label),
        };
        if let Some(doc) = &self.doc {
            if self.is_builtin {
                label = format!("{}\n---\n\n{}\n", label, doc);
            } else {
                label = format!("{}\n---\n\n<pre>\n{}\n</pre>\n", label, doc);
            }
        }
        // print!("{}", &label);
        label
    }

    pub(crate) fn make_label(&self) -> String {
        let format_params = |params: &[Param]| {
            params
                .iter()
                .map(|p| match &p.default {
                    Some(d) => format!("{}={}", p.name, d),
                    None => p.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };

        match &self.kind {
            ItemKind::Variable => self.name.to_owned(),
            ItemKind::Function { flags: _, params } => {
                format!("{}({})", self.name, format_params(params))
            }
            ItemKind::Keyword(_) => self.name.clone(),
            ItemKind::Module { params, .. } => {
                format!("{}({})", self.name, format_params(params))
            }
        }
    }

    pub(crate) fn parse(code: &str, node: &Node) -> Option<Self> {
        lazy_static! {
            static ref FLAG_RE: Regex =
                Regex::new(r"(?m)builtin_flags\((?P<flags>[01]{16})\)").unwrap();
        };

        let extract_name = |name| {
            node.child_by_field_name(name)
                .map(|child| node_text(code, &child).to_owned())
        };

        match node.kind() {
            "module_declaration" => {
                let flags: u16 = if let Some(child) = node
                    .child_by_field_name("body")
                    .and_then(|body| body.named_child(0))
                {
                    let body = node_text(code, &child);
                    if let Some(cap) = &FLAG_RE.captures(body) {
                        let flag_str = &cap["flags"];
                        u16::from_str_radix(flag_str, 2).unwrap()
                    } else {
                        0
                    }
                } else {
                    0
                };
                Some(Self {
                    name: extract_name("name")?,
                    kind: ItemKind::Module {
                        flags,
                        params: node
                            .child_by_field_name("parameters")
                            .map_or(vec![], |params| Param::parse_declaration(code, &params)),
                    },
                    range: node.lsp_range(),
                    ..Default::default()
                })
            }
            "function_declaration" => {
                let flags = if let Some(child) = node.children(&mut node.walk()).last() {
                    let body = node_text(code, &child);
                    if let Some(cap) = &FLAG_RE.captures(body) {
                        let flag_str = &cap["flags"];
                        u16::from_str_radix(flag_str, 2).unwrap()
                    } else {
                        0
                    }
                } else {
                    0
                };
                Some(Self {
                    name: extract_name("name")?,
                    kind: ItemKind::Function {
                        flags,
                        params: node
                            .child_by_field_name("parameters")
                            .map_or(vec![], |params| Param::parse_declaration(code, &params)),
                    },
                    range: node.lsp_range(),
                    ..Default::default()
                })
            }
            "assignment" => Some(Self {
                name: extract_name("left")?,
                kind: ItemKind::Variable,
                range: node.lsp_range(),
                ..Default::default()
            }),
            _ => None,
        }
    }

    pub(crate) fn get_symbol_kind(&self) -> SymbolKind {
        match self.kind {
            ItemKind::Function { .. } => SymbolKind::FUNCTION,
            ItemKind::Module { .. } => SymbolKind::MODULE,
            ItemKind::Variable => SymbolKind::VARIABLE,
            ItemKind::Keyword(_) => SymbolKind::KEY,
        }
    }
}
