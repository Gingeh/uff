use crate::config::home;
use anyhow::{Context, Result, bail, ensure};
use kdl::{KdlDocument, KdlNode};
use std::path::PathBuf;

#[derive(Debug)]
pub struct Menu {
    pub fuzzel_args: Vec<String>,
    pub fuzzel_config: Vec<(String, String)>,
    pub icon_dirs: Vec<PathBuf>,
    pub items: Vec<Item>,
    pub sort: bool,
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

pub fn parse_config(src: &str) -> Result<Menu> {
    let doc = src
        .parse::<KdlDocument>()
        .context("failed to parse KDL document")?;
    parse_menu_from_nodes(doc.nodes())
}

fn parse_menu_from_nodes(nodes: &[KdlNode]) -> Result<Menu> {
    let mut fuzzel_args = Vec::new();
    let mut fuzzel_config = Vec::new();
    let mut icon_dirs = Vec::new();
    let mut items = Vec::new();
    let mut sort = true;

    for node in nodes {
        match node.name().value() {
            "fuzzel-args" => {
                fuzzel_args.clear();
                ensure!(
                    node.children().is_none(),
                    "fuzzel-args must not have children"
                );
                for entry in node.entries() {
                    ensure!(
                        entry.name().is_none(),
                        "fuzzel-args arguments must not be named"
                    );
                    ensure!(
                        entry.value().is_string(),
                        "fuzzel-args arguments must be strings"
                    );
                    fuzzel_args.push(entry.value().as_string().unwrap().to_owned());
                }
            }
            "fuzzel-config" => {
                fuzzel_config.clear();
                ensure!(
                    node.entries().is_empty(),
                    "fuzzel-config must not have arguments, only children"
                );
                let children = node
                    .children()
                    .context("fuzzel-config must have children")?;
                for kv in children.nodes() {
                    let key = kv.name().value().to_owned();
                    ensure!(
                        kv.entries().len() == 1,
                        "fuzzel-config key must have exactly one argument"
                    );
                    ensure!(
                        kv.entries()[0].name().is_none(),
                        "fuzzel-config key argument must not be named"
                    );
                    ensure!(
                        kv.entries()[0].value().is_string(),
                        "fuzzel-config key argument must be a string"
                    );
                    let value = kv.entries()[0].value().as_string().unwrap().to_owned();
                    fuzzel_config.push((key, value));
                }
            }
            "icon-dir" => {
                ensure!(node.children().is_none(), "icon-dir must not have children");
                ensure!(
                    node.entries().len() == 1,
                    "icon-dir must have exactly one argument"
                );
                ensure!(
                    node.entries()[0].name().is_none(),
                    "icon-dir argument must not be named"
                );
                ensure!(
                    node.entries()[0].value().is_string(),
                    "icon-dir argument must be a string"
                );
                let path_str = node.entries()[0]
                    .value()
                    .as_string()
                    .unwrap()
                    .replace('~', &home());
                let path = PathBuf::from(path_str);
                ensure!(path.is_absolute(), "icon-dir path must be absolute");
                icon_dirs.push(path);
            }
            "menu" | "program" => {
                ensure!(
                    node.entries().len() == 1,
                    "item must have exactly one argument"
                );
                ensure!(
                    node.entries()[0].name().is_none(),
                    "item name must not be a named argument"
                );
                ensure!(
                    node.entries()[0].value().is_string(),
                    "item name must be a string"
                );
                let name = node.entries()[0].value().as_string().unwrap();
                let children = node.children().context("item must have children")?.nodes();
                items.push(parse_item_from_nodes(node.name().value(), name, children)?);
            }
            "no-sort" => {
                ensure!(node.children().is_none(), "no-sort must not have children");
                ensure!(node.entries().is_empty(), "no-sort must not have arguments");
                sort = false;
            }
            "icon" => {} // already parsed by parse_item_from_nodes
            other => anyhow::bail!("unexpected node in menu: {}", other),
        }
    }

    Ok(Menu {
        fuzzel_args,
        fuzzel_config,
        icon_dirs,
        items,
        sort,
    })
}

fn parse_program_from_nodes(nodes: &[KdlNode]) -> Result<Program> {
    let mut command: Vec<String> = Vec::new();

    for node in nodes {
        match node.name().value() {
            "command" => {
                command.clear();
                ensure!(node.children().is_none(), "command must not have children");
                for entry in node.entries() {
                    ensure!(
                        entry.name().is_none(),
                        "command arguments must not be named"
                    );
                    ensure!(
                        entry.value().is_string(),
                        "command arguments must be strings"
                    );
                    command.push(entry.value().as_string().unwrap().to_owned());
                }
            }
            "icon" => {} // already parsed by parse_item_from_nodes
            other => bail!("unexpected node in program: {}", other),
        }
    }

    Ok(Program { command })
}

fn parse_item_from_nodes(kind: &str, name: &str, nodes: &[KdlNode]) -> Result<Item> {
    let mut icon: Option<String> = None;

    for node in nodes {
        if node.name().value() == "icon" {
            ensure!(node.children().is_none(), "icon must not have children");
            ensure!(
                node.entries().len() == 1,
                "icon must have exactly one argument"
            );
            ensure!(
                node.entries()[0].name().is_none(),
                "icon argument must not be named"
            );
            ensure!(
                node.entries()[0].value().is_string(),
                "icon argument must be a string"
            );
            icon = Some(node.entries()[0].value().as_string().unwrap().to_owned());
        }
    }

    let contents = match kind {
        "menu" => ItemContents::Menu(parse_menu_from_nodes(nodes)?),
        "program" => ItemContents::Program(parse_program_from_nodes(nodes)?),
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
