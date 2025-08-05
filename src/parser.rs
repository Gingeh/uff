use crate::config::home;
use kdl::{KdlDocument, KdlNode};
use log::warn;
use miette::{Diagnostic, LabeledSpan, Result, SourceSpan, miette};
use std::{fmt::Debug, path::PathBuf};
use thiserror::Error;

#[derive(Debug)]
pub struct Menu {
    pub fuzzel_args: Vec<String>,
    pub fuzzel_config: Vec<(String, String)>,
    pub icon_dirs: Vec<PathBuf>,
    pub items: Vec<Item>,
}

#[derive(Debug)]
pub struct Item {
    pub name: String,
    pub icon: Option<String>,
    pub contents: ItemContents,
}

#[derive(Debug)]
pub enum ItemContents {
    Menu(Menu),
    Program(Program),
}

#[derive(Debug)]
pub struct Program {
    pub command: Vec<String>,
}

// This is used to remove the default unnamed source from a KdlDiagnostic
// so it can be replaced with a named source.
#[derive(Debug, Error)]
#[error(transparent)]
struct KdlDiagnosticWrapper(kdl::KdlDiagnostic);
impl Diagnostic for KdlDiagnosticWrapper {
    fn labels<'a>(&'a self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + 'a>> {
        self.0.labels()
    }
    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.0.help()
    }
}

pub fn parse_config(src: &str) -> Result<Menu> {
    let doc = src.parse::<KdlDocument>().map_err(|e| {
        let original = e.diagnostics[0].clone();
        KdlDiagnosticWrapper(original)
    })?;
    parse_menu_from_nodes(&doc)
}

fn no_parameters(node: &KdlNode) -> Result<()> {
    for entry in node.entries() {
        if let Some(name) = entry.name() {
            return Err(miette!(
                labels = vec![LabeledSpan::new_primary_with_span(
                    Some("remove this name".to_string()),
                    name.span(),
                )],
                "{} should not have any named parameters",
                node.name().value().to_owned(),
            ));
        }
    }
    Ok(())
}

fn no_arguments(node: &KdlNode) -> Result<()> {
    if let Some(first) = node.entries().first() {
        let last = node.entries().last().unwrap().span();
        let full_span = SourceSpan::new(
            first.span().offset().into(),
            (last.offset() + last.len()) - first.span().offset(),
        );
        return Err(miette!(
            labels = vec![LabeledSpan::new_primary_with_span(
                Some("these".to_string()),
                full_span
            )],
            "{} should not have any arguments",
            node.name().value().to_owned(),
        ));
    }
    Ok(())
}

fn no_children(node: &KdlNode) -> Result<()> {
    if let Some(children) = node.children() {
        let this = if children.nodes().len() < 2 {
            "remove this".to_string()
        } else {
            "remove these".to_string()
        };
        return Err(miette!(
            labels = vec![LabeledSpan::new_primary_with_span(
                Some(this),
                children.span()
            )],
            "{} should not have any children",
            node.name().value().to_owned(),
        ));
    }
    Ok(())
}

fn one_argument(node: &KdlNode) -> Result<String> {
    if node.entries().len() != 1 {
        let labeled_span = if node.entries().is_empty() {
            let after_node = node.name().span().offset() + node.name().span().len();
            LabeledSpan::new_primary_with_span(
                Some("here".to_string()),
                SourceSpan::new(after_node.into(), 0),
            )
        } else {
            let first = node.entries()[1].span();
            let last = node.entries().last().unwrap().span();
            let full_span = SourceSpan::new(
                first.offset().into(),
                (last.offset() + last.len()) - first.offset(),
            );
            let these = if node.entries().len() < 3 {
                "remove this".to_string()
            } else {
                "remove these".to_string()
            };
            LabeledSpan::new_primary_with_span(Some(these), full_span)
        };
        return Err(miette!(
            labels = vec![labeled_span],
            "{} should have exactly one argument",
            node.name().value().to_owned(),
        ));
    }

    let Some(argument) = node.entries()[0].value().as_string() else {
        return Err(miette!(
            labels = vec![LabeledSpan::new_primary_with_span(
                Some("this".to_string()),
                node.entries()[0].span()
            )],
            help = "try wrapping it in quotes",
            "argument should be a string",
        ));
    };

    Ok(argument.to_owned())
}

fn many_arguments(node: &KdlNode) -> Result<Vec<String>> {
    if node.entries().is_empty() {
        let after_node = node.name().span().offset() + node.name().span().len();
        return Err(miette!(
            labels = vec![LabeledSpan::new_primary_with_span(
                Some("here".to_string()),
                SourceSpan::new(after_node.into(), 0),
            )],
            "{} should have arguments",
            node.name().value().to_owned(),
        ));
    }

    let mut args = Vec::new();
    for entry in node.entries() {
        if let Some(value) = entry.value().as_string() {
            args.push(value.to_owned());
        } else {
            return Err(miette!(
                labels = vec![LabeledSpan::new_primary_with_span(
                    Some("this".to_string()),
                    entry.span()
                )],
                help = "try wrapping it in quotes",
                "argument should be a string",
            ));
        }
    }
    Ok(args)
}

fn children(node: &KdlNode) -> Result<&KdlDocument> {
    node.children().ok_or_else(|| {
        let after_entries = if node.entries().is_empty() {
            node.name().span().offset() + node.name().span().len()
        } else {
            let last_entry = node.entries().last().unwrap();
            last_entry.span().offset() + last_entry.span().len()
        };
        miette!(
            labels = vec![LabeledSpan::new_primary_with_span(
                Some("here".to_string()),
                SourceSpan::new(after_entries.into(), 0),
            )],
            "{} should have children",
            node.name().value().to_owned(),
        )
    })
}

fn parse_menu_from_nodes(doc: &KdlDocument) -> Result<Menu> {
    let mut fuzzel_args = Vec::new();
    let mut fuzzel_config = Vec::new();
    let mut icon_dirs = Vec::new();
    let mut items = Vec::new();

    for node in doc.nodes() {
        match node.name().value() {
            "fuzzel-args" => {
                if !fuzzel_args.is_empty() {
                    warn!("fuzzel-args already defined, overwriting");
                }
                fuzzel_args = many_arguments(node)?;
                no_parameters(node)?;
                no_children(node)?;
            }
            "fuzzel-config" => {
                if !fuzzel_config.is_empty() {
                    warn!("fuzzel-config already defined, overwriting");
                    fuzzel_config.clear();
                }
                let children = children(node)?;
                for kv in children.nodes() {
                    let key = kv.name().value().to_owned();
                    let value = one_argument(kv)?;
                    fuzzel_config.push((key, value));
                    no_parameters(kv)?;
                }
                no_arguments(node)?;
            }
            "icon-dir" => {
                let path_str = one_argument(node)?;
                let path = PathBuf::from(path_str.replace('~', &home()));
                if !path.is_absolute() {
                    warn!(
                        "relative icon-dirs can behave unexpectedly, consider using absolute paths"
                    );
                }
                icon_dirs.push(path);
                no_parameters(node)?;
                no_children(node)?;
            }
            "menu" | "program" => {
                let name = one_argument(node)?;
                let children = children(node)?;
                items.push(parse_item_from_nodes(node.name().value(), &name, children)?);
                no_parameters(node)?;
            }
            "icon" => {} // already parsed by parse_item_from_nodes
            other => {
                return Err(miette!(
                    labels = vec![LabeledSpan::new_primary_with_span(
                        Some("this".to_string()),
                        node.span()
                    )],
                    "unexpected node in menu: {}",
                    other,
                ));
            }
        }
    }

    Ok(Menu {
        fuzzel_args,
        fuzzel_config,
        icon_dirs,
        items,
    })
}

fn parse_program_from_nodes(doc: &KdlDocument) -> Result<Program> {
    let mut command: Vec<String> = Vec::new();

    for node in doc.nodes() {
        match node.name().value() {
            "command" => {
                if !command.is_empty() {
                    warn!("command already defined, overwriting");
                }
                command = many_arguments(node)?;
                no_parameters(node)?;
                no_children(node)?;
            }
            "icon" => {} // already parsed by parse_item_from_nodes
            other => {
                return Err(miette!(
                    labels = vec![LabeledSpan::new_primary_with_span(
                        Some("this".to_string()),
                        node.span()
                    )],
                    "unexpected node in program: {}",
                    other,
                ));
            }
        }
    }

    if command.is_empty() {
        return Err(miette!(
            labels = vec![LabeledSpan::new_primary_with_span(
                Some("here".to_string()),
                doc.span(),
            )],
            "program should have a command",
        ));
    }

    Ok(Program { command })
}

fn parse_item_from_nodes(kind: &str, name: &str, doc: &KdlDocument) -> Result<Item> {
    let mut icon: Option<String> = None;

    for node in doc.nodes() {
        if node.name().value() == "icon" {
            if icon.is_some() {
                warn!("icon already defined, overwriting");
            }
            icon = Some(one_argument(node)?);
            no_parameters(node)?;
            no_children(node)?;
        }
    }

    let contents = match kind {
        "menu" => ItemContents::Menu(parse_menu_from_nodes(doc)?),
        "program" => ItemContents::Program(parse_program_from_nodes(doc)?),
        _ => unreachable!(),
    };

    Ok(Item {
        name: name.to_owned(),
        icon,
        contents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_phase_comprehensive() {
        // Test simple program parsing
        let simple_config = r#"
            program "Item1" {
                command "cmd1"
            }
        "#;
        let simple = parse_config(simple_config).unwrap();
        assert_eq!(simple.items.len(), 1);
        assert_eq!(simple.items[0].name, "Item1");
        if let ItemContents::Program(ref prog) = simple.items[0].contents {
            assert_eq!(prog.command, vec!["cmd1"]);
        } else {
            panic!("Expected program item");
        }

        // Test parsing with fuzzel config
        let config_with_fuzzel = r#"
            fuzzel-args "--arg1" "--arg2"
            fuzzel-config {
                key1 "value1"
                key2 "value2"
            }
            program "Item1" {
                icon "icon1"
                command "cmd1"
            }
        "#;
        let with_config = parse_config(config_with_fuzzel).unwrap();
        assert_eq!(with_config.fuzzel_args, vec!["--arg1", "--arg2"]);
        assert_eq!(
            with_config.fuzzel_config,
            vec![
                ("key1".to_string(), "value1".to_string()),
                ("key2".to_string(), "value2".to_string()),
            ]
        );
        assert_eq!(with_config.items[0].icon, Some("icon1".to_string()));

        // Test nested menu parsing
        let nested_config = r#"
            program "Item1" {
                command "cmd1"
            }
            menu "Submenu1" {
                fuzzel-config {
                    subkey "subvalue"
                }
                program "Item2" {
                    command "cmd2" "arg2"
                }
            }
        "#;
        let nested = parse_config(nested_config).unwrap();
        assert_eq!(nested.items.len(), 2);
        assert_eq!(nested.items[0].name, "Item1");
        assert_eq!(nested.items[1].name, "Submenu1");
        if let ItemContents::Menu(ref submenu) = nested.items[1].contents {
            assert_eq!(
                submenu.fuzzel_config,
                vec![("subkey".to_string(), "subvalue".to_string())]
            );
            assert_eq!(submenu.items.len(), 1);
            assert_eq!(submenu.items[0].name, "Item2");
            if let ItemContents::Program(ref prog) = submenu.items[0].contents {
                assert_eq!(prog.command, vec!["cmd2", "arg2"]);
            } else {
                panic!("Expected program in submenu");
            }
        } else {
            panic!("Expected menu item");
        }
    }
}
