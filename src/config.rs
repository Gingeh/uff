use anyhow::{Context, Result};
use bitcode::{Decode, Encode};
use log::{error, info, warn};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    ffi::OsStr,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

use crate::parser::{self, ItemContents, Menu};

pub fn default_config_dir() -> PathBuf {
    let mut path;
    if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
        path = PathBuf::from(config_home);
    } else {
        path = std::env::home_dir().unwrap();
        path.push(".config");
    }
    path
}

pub fn default_config_path() -> PathBuf {
    let mut path = default_config_dir();
    path.push(env!("CARGO_BIN_NAME"));
    path.push("default.kdl");
    info!("using default config path");
    path
}

#[derive(Encode, Decode, Debug)]
pub struct ComputedConfig {
    /// First 8 bytes of SHA256 digest of raw config file.
    hash: [u8; 8],
    pub initial_menu: ComputedMenu,
    pub items: Vec<ComputedItem>,
}

#[derive(Encode, Decode, Debug)]
pub enum ComputedItem {
    Menu(ComputedMenu),
    Program(ComputedProgram),
}

#[derive(Encode, Decode, Debug)]
pub struct ComputedMenu {
    pub args: Vec<String>,
    pub input: Vec<u8>,
    pub items_offset: usize,
}

#[derive(Encode, Decode, Debug, Clone)]
pub struct ComputedProgram {
    pub command: Vec<String>,
}

struct IdGenerator {
    counter: usize,
}

impl IdGenerator {
    const fn new() -> Self {
        Self { counter: 0 }
    }

    const fn next_id(&mut self) -> usize {
        let id = self.counter;
        self.counter += 1;
        id
    }
}

#[derive(Clone)]
struct InheritanceFrame {
    icon_dirs: Vec<PathBuf>,
    fuzzel_config_id: Option<usize>,
}

// Intermediate tree structure that holds fully resolved data
#[derive(Debug)]
struct ResolvedMenu {
    args: Vec<String>,
    input: Vec<u8>,
    items: Vec<ResolvedItem>,
}

#[derive(Debug)]
enum ResolvedItem {
    Menu(ResolvedMenu),
    Program(ComputedProgram),
}

impl InheritanceFrame {
    fn default() -> Self {
        let mut data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
        if data_dirs.is_empty() {
            data_dirs = "/usr/local/share/:/usr/share/".to_string();
            info!("XDG_DATA_DIRS is empty, using {data_dirs} as default");
        }

        let mut icon_dirs: Vec<PathBuf> = std::env::split_paths(&data_dirs).collect();

        let mut data_home = std::env::var("XDG_DATA_HOME").unwrap_or_default();
        if data_home.is_empty() {
            let home = std::env::home_dir().unwrap();
            data_home = format!("{}/.local/share/", home.display());
            warn!("XDG_DATA_HOME is empty, using {data_home} as default");
        }
        icon_dirs.push(PathBuf::from(data_home));

        Self {
            icon_dirs,
            fuzzel_config_id: None,
        }
    }
}

pub fn get_computed_config(path: &Path) -> Result<ComputedConfig> {
    let config_string = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    let actual_hash = Sha256::digest(&config_string);

    let preset_name = path
        .file_stem()
        .unwrap()
        .to_str()
        .context("preset name contains non-utf8 characters")?;
    let cache_path = make_cache_path(preset_name);
    let maybe_cached_config = read_cached_config(&cache_path);

    match maybe_cached_config {
        Some(cached_config) => {
            if cached_config.hash == actual_hash[..8] {
                info!("using cached config");
                return Ok(cached_config);
            }
            info!("cached config is stale, rebuilding");
        }
        None => {
            info!("no cached config, building from scratch");
        }
    }

    let computed_config = compute_config(&config_string, actual_hash.as_slice(), preset_name)?;
    cache_config(&cache_path, &computed_config);
    Ok(computed_config)
}

fn make_cache_path(preset_name: &str) -> PathBuf {
    let mut cache_path = get_cache_dir();
    cache_path.push(preset_name);
    cache_path.set_extension("cache");
    cache_path
}

fn make_fuzzel_config_path(id: usize, preset_name: &str) -> PathBuf {
    let mut config_path = get_cache_dir();
    config_path.push(format!("{preset_name}{id}"));
    config_path.set_extension("fuzzel.ini");
    config_path
}

fn make_fuzzel_cache_path(id: usize, preset_name: &str) -> PathBuf {
    let mut cache_path = get_cache_dir();
    cache_path.push(format!("{preset_name}{id}"));
    cache_path.set_extension("fuzzel.cache");
    cache_path
}

fn default_fuzzel_config_path() -> PathBuf {
    if cfg!(test) {
        PathBuf::from("placeholder.fuzzel.ini")
    } else {
        let mut path = default_config_dir();
        path.push("fuzzel");
        path.push("fuzzel.ini");
        path
    }
}

fn create_fuzzel_config(
    pairs: &[(String, String)],
    id: usize,
    inherit_id: Option<usize>,
    preset_name: &str,
) -> PathBuf {
    let config_path = make_fuzzel_config_path(id, preset_name);

    // Create the directory if it doesn't exist
    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut config_file = File::create(&config_path).unwrap();

    let inherit_path = inherit_id.map_or_else(default_fuzzel_config_path, |inherit_id| {
        make_fuzzel_config_path(inherit_id, preset_name)
    });
    writeln!(&mut config_file, "include={}", inherit_path.display()).unwrap();

    for (key, value) in pairs {
        writeln!(&mut config_file, "{key}={value}").unwrap();
    }

    config_path
}

fn get_cache_dir() -> PathBuf {
    if cfg!(test) {
        PathBuf::from("./target/test-cache")
    } else {
        let mut path;
        if let Ok(config_home) = std::env::var("XDG_CACHE_HOME") {
            path = PathBuf::from(config_home);
        } else {
            path = std::env::home_dir().unwrap();
            path.push(".cache");
        }
        path.push(env!("CARGO_BIN_NAME"));
        path
    }
}

fn read_cached_config(path: &Path) -> Option<ComputedConfig> {
    let bytes = std::fs::read(path).ok()?;
    let decoded = bitcode::decode(&bytes);
    match decoded {
        Ok(decoded) => Some(decoded),
        Err(error) => {
            error!("failed to decode cached config: {error}");
            None
        }
    }
}

fn cache_config(path: &Path, computed_config: &ComputedConfig) {
    let bytes = bitcode::encode(computed_config);
    if let Err(error) = std::fs::create_dir_all(path.parent().unwrap()) {
        error!("failed to create cache directory: {error}");
    }
    if let Err(error) = std::fs::write(path, bytes) {
        error!("failed to write cache file: {error}");
    }
}

fn compute_config(config_string: &str, hash: &[u8], preset_name: &str) -> Result<ComputedConfig> {
    let config = parser::parse_config(config_string)?;
    let inheritance_stack = vec![InheritanceFrame::default()];
    let mut id_gen = IdGenerator::new();

    // Build phase: create fully resolved tree with inheritance applied
    let resolved_menu = build_resolved_menu(&config, &inheritance_stack, &mut id_gen, preset_name);

    let mut items = Vec::new();
    // Flatten phase: convert tree to a flat list
    let initial_menu = flatten_resolved_menu(&resolved_menu, &mut items);

    Ok(ComputedConfig {
        hash: std::array::from_fn(|i| hash[i]),
        initial_menu,
        items,
    })
}

fn build_resolved_menu(
    menu: &Menu,
    inheritance_stack: &[InheritanceFrame],
    id_gen: &mut IdGenerator,
    preset_name: &str,
) -> ResolvedMenu {
    let id = id_gen.next_id();

    let mut args = menu.fuzzel_args.clone();

    let last_config = inheritance_stack
        .iter()
        .filter_map(|frame| frame.fuzzel_config_id)
        .next_back();

    if menu.fuzzel_config.is_empty() {
        if let Some(last_config) = last_config {
            args.push("--config".to_string());
            args.push(
                make_fuzzel_config_path(last_config, preset_name)
                    .display()
                    .to_string(),
            );
        }
    } else {
        args.push("--config".to_string());
        args.push(
            create_fuzzel_config(&menu.fuzzel_config, id, last_config, preset_name)
                .display()
                .to_string(),
        );
    }

    // Add unique cache path for this menu
    args.push("--cache".to_string());
    args.push(
        make_fuzzel_cache_path(id, preset_name)
            .display()
            .to_string(),
    );

    // Build icon dirs with inheritance
    let icon_dirs: VecDeque<&Path> = menu
        .icon_dirs
        .iter()
        .map(PathBuf::as_path)
        .chain(
            inheritance_stack
                .iter()
                .rev()
                .flat_map(|frame| frame.icon_dirs.iter().map(PathBuf::as_path)),
        )
        .collect();

    // Build fuzzel input format: {NAME}\0icon\x1f{ICON_PATH}\n
    let mut input = Vec::new();
    for item in &menu.items {
        write!(&mut input, "{}", item.name).unwrap();
        if let Some(icon) = &item.icon {
            let mut item_icon_dirs = icon_dirs.clone();
            if let ItemContents::Menu(menu) = &item.contents {
                for icon_dir in &menu.icon_dirs {
                    item_icon_dirs.push_front(icon_dir);
                }
            }

            let icon_path = search_for_icon(icon, item_icon_dirs).map_or_else(
                || icon.replace('~', &home()),
                |path| path.display().to_string(),
            );
            write!(&mut input, "\0icon\x1f{icon_path}").unwrap();
        }
        writeln!(&mut input).unwrap();
    }

    // Build child inheritance frame for recursive calls
    let child_frame = InheritanceFrame {
        icon_dirs: menu.icon_dirs.clone(),
        fuzzel_config_id: if menu.fuzzel_config.is_empty() {
            None
        } else {
            Some(id)
        },
    };

    // Recursively build resolved items
    let mut resolved_items = Vec::new();
    for item in &menu.items {
        match &item.contents {
            ItemContents::Menu(child_menu) => {
                let mut child_inheritance_stack = inheritance_stack.to_vec();
                child_inheritance_stack.push(child_frame.clone());
                let resolved_child =
                    build_resolved_menu(child_menu, &child_inheritance_stack, id_gen, preset_name);
                resolved_items.push(ResolvedItem::Menu(resolved_child));
            }
            ItemContents::Program(program) => {
                resolved_items.push(ResolvedItem::Program(ComputedProgram {
                    command: program.command.clone(),
                }));
            }
        }
    }

    ResolvedMenu {
        args,
        input,
        items: resolved_items,
    }
}

fn flatten_resolved_menu(
    resolved_menu: &ResolvedMenu,
    items: &mut Vec<ComputedItem>,
) -> ComputedMenu {
    let items_offset = items.len();

    // First pass: add all direct children to maintain adjacency
    for resolved_item in &resolved_menu.items {
        match resolved_item {
            ResolvedItem::Menu(child_menu) => {
                // Add placeholder menu item - we'll update its offset in second pass
                items.push(ComputedItem::Menu(ComputedMenu {
                    args: child_menu.args.clone(),
                    input: child_menu.input.clone(),
                    items_offset: 0, // Will be updated below
                }));
            }
            ResolvedItem::Program(program) => {
                items.push(ComputedItem::Program(program.clone()));
            }
        }
    }

    // Second pass: recursively flatten submenus and update their offsets
    let mut current_index = items_offset;
    for resolved_item in &resolved_menu.items {
        if let ResolvedItem::Menu(child_menu) = resolved_item {
            let child_offset = items.len();
            // Update the offset for this menu item
            if let ComputedItem::Menu(computed_menu) = &mut items[current_index] {
                computed_menu.items_offset = child_offset;
            }
            // Recursively flatten the child menu
            flatten_resolved_menu(child_menu, items);
        }
        current_index += 1;
    }

    ComputedMenu {
        args: resolved_menu.args.clone(),
        input: resolved_menu.input.clone(),
        items_offset,
    }
}

pub fn home() -> String {
    let home_path = std::env::home_dir().unwrap();
    home_path.to_string_lossy().to_string()
}

fn search_for_icon<'a>(name: &str, dirs: impl IntoIterator<Item = &'a Path>) -> Option<PathBuf> {
    if name.contains('/') {
        info!("icon name contains a '/', treating as full path: {name}");
        return None; // probably a full path
    }

    for dir in dirs {
        for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
            if entry.path().file_stem() == Some(OsStr::new(name))
                && (entry.path().extension() == Some(OsStr::new("png"))
                    || entry.path().extension() == Some(OsStr::new("svg")))
            {
                return Some(entry.into_path());
            }
        }
    }
    error!("icon '{name}' not found in specified directories");
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Item, ItemContents, Menu, Program};

    #[test]
    fn test_build_phase_comprehensive() {
        // Test simple menu building
        let simple_menu = Menu {
            fuzzel_args: vec!["--arg1".to_string()],
            fuzzel_config: vec![],
            icon_dirs: vec![],
            items: vec![Item {
                name: "Item1".to_string(),
                icon: None,
                contents: ItemContents::Program(Program {
                    command: vec!["cmd1".to_string()],
                }),
            }],
        };
        let inheritance_stack = vec![InheritanceFrame::default()];
        let mut id_gen = IdGenerator::new();
        let simple_result =
            build_resolved_menu(&simple_menu, &inheritance_stack, &mut id_gen, "testsimple");
        assert_eq!(
            simple_result.args,
            vec![
                "--arg1",
                "--cache",
                "./target/test-cache/testsimple0.fuzzel.cache"
            ]
        );
        assert_eq!(simple_result.input, b"Item1\n");
        assert_eq!(simple_result.items.len(), 1);

        // Test menu with config file generation
        let menu_with_config = Menu {
            fuzzel_args: vec![],
            fuzzel_config: vec![("width".to_string(), "12".to_string())],
            icon_dirs: vec![],
            items: vec![Item {
                name: "Item1".to_string(),
                icon: None,
                contents: ItemContents::Program(Program {
                    command: vec!["cmd1".to_string()],
                }),
            }],
        };
        let config_result = build_resolved_menu(
            &menu_with_config,
            &inheritance_stack,
            &mut id_gen,
            "testconfig",
        );
        assert_eq!(
            config_result.args,
            vec![
                "--config",
                "./target/test-cache/testconfig1.fuzzel.ini",
                "--cache",
                "./target/test-cache/testconfig1.fuzzel.cache"
            ]
        );

        // Verify config file was created with correct content
        let config_content =
            std::fs::read_to_string("./target/test-cache/testconfig1.fuzzel.ini").unwrap();
        assert_eq!(config_content, "include=placeholder.fuzzel.ini\nwidth=12\n");

        // Test nested menu with inheritance
        let nested_menu = Menu {
            fuzzel_args: vec!["--base-arg".to_string()],
            fuzzel_config: vec![("base_key".to_string(), "base_value".to_string())],
            icon_dirs: vec![],
            items: vec![
                Item {
                    name: "Item1".to_string(),
                    icon: None,
                    contents: ItemContents::Program(Program {
                        command: vec!["cmd1".to_string()],
                    }),
                },
                Item {
                    name: "Submenu1".to_string(),
                    icon: None,
                    contents: ItemContents::Menu(Menu {
                        fuzzel_args: vec![],
                        fuzzel_config: vec![("sub_key".to_string(), "sub_value".to_string())],
                        icon_dirs: vec![],
                        items: vec![Item {
                            name: "Item2".to_string(),
                            icon: None,
                            contents: ItemContents::Program(Program {
                                command: vec!["cmd2".to_string()],
                            }),
                        }],
                    }),
                },
            ],
        };
        let nested_result =
            build_resolved_menu(&nested_menu, &inheritance_stack, &mut id_gen, "testnested");

        // Check top-level menu
        assert_eq!(
            nested_result.args,
            vec![
                "--base-arg",
                "--config",
                "./target/test-cache/testnested2.fuzzel.ini",
                "--cache",
                "./target/test-cache/testnested2.fuzzel.cache"
            ]
        );
        assert_eq!(nested_result.input, b"Item1\nSubmenu1\n");
        assert_eq!(nested_result.items.len(), 2);

        // Check nested submenu
        if let ResolvedItem::Menu(ref submenu) = nested_result.items[1] {
            assert_eq!(
                submenu.args,
                vec![
                    "--config",
                    "./target/test-cache/testnested3.fuzzel.ini",
                    "--cache",
                    "./target/test-cache/testnested3.fuzzel.cache"
                ]
            );
            assert_eq!(submenu.input, b"Item2\n");
        } else {
            panic!("Expected nested menu");
        }

        // Verify inheritance in config files
        let base_config =
            std::fs::read_to_string("./target/test-cache/testnested2.fuzzel.ini").unwrap();
        assert_eq!(
            base_config,
            "include=placeholder.fuzzel.ini\nbase_key=base_value\n"
        );

        let sub_config =
            std::fs::read_to_string("./target/test-cache/testnested3.fuzzel.ini").unwrap();
        assert_eq!(
            sub_config,
            "include=./target/test-cache/testnested2.fuzzel.ini\nsub_key=sub_value\n"
        );
    }

    #[test]
    fn test_flatten_phase_comprehensive() {
        // Test simple menu flattening
        let simple_resolved = ResolvedMenu {
            args: vec!["--arg1".to_string()],
            input: b"Item1\n".to_vec(),
            items: vec![ResolvedItem::Program(ComputedProgram {
                command: vec!["cmd1".to_string()],
            })],
        };
        let mut simple_items = Vec::new();
        let simple_flattened = flatten_resolved_menu(&simple_resolved, &mut simple_items);

        assert_eq!(simple_flattened.args, vec!["--arg1"]);
        assert_eq!(simple_flattened.input, b"Item1\n");
        assert_eq!(simple_flattened.items_offset, 0);
        assert_eq!(simple_items.len(), 1);
        if let ComputedItem::Program(ref prog) = simple_items[0] {
            assert_eq!(prog.command, vec!["cmd1"]);
        } else {
            panic!("Expected program item");
        }

        // Test nested menu flattening with proper adjacency preservation
        let nested_submenu = ResolvedMenu {
            args: vec!["--sub-arg".to_string()],
            input: b"Item2\n".to_vec(),
            items: vec![ResolvedItem::Program(ComputedProgram {
                command: vec!["cmd2".to_string()],
            })],
        };
        let nested_resolved = ResolvedMenu {
            args: vec!["--base-arg".to_string()],
            input: b"Item1\nSubmenu1\n".to_vec(),
            items: vec![
                ResolvedItem::Program(ComputedProgram {
                    command: vec!["cmd1".to_string()],
                }),
                ResolvedItem::Menu(nested_submenu),
            ],
        };

        let mut nested_items = Vec::new();
        let nested_flattened = flatten_resolved_menu(&nested_resolved, &mut nested_items);

        // Check flattened menu structure
        assert_eq!(nested_flattened.args, vec!["--base-arg"]);
        assert_eq!(nested_flattened.input, b"Item1\nSubmenu1\n");
        assert_eq!(nested_flattened.items_offset, 0);

        // Check flattened items preserve adjacency
        assert_eq!(nested_items.len(), 3);
        if let ComputedItem::Program(ref prog) = nested_items[0] {
            assert_eq!(prog.command, vec!["cmd1"]);
        } else {
            panic!("Expected first program item");
        }
        if let ComputedItem::Menu(ref menu) = nested_items[1] {
            assert_eq!(menu.args, vec!["--sub-arg"]);
            assert_eq!(menu.input, b"Item2\n");
            assert_eq!(menu.items_offset, 2); // Points to next item
        } else {
            panic!("Expected menu item");
        }
        if let ComputedItem::Program(ref prog) = nested_items[2] {
            assert_eq!(prog.command, vec!["cmd2"]);
        } else {
            panic!("Expected second program item");
        }

        // Test escaped input handling
        let escaped_resolved = ResolvedMenu {
            args: vec![],
            input: b"Item1\0icon\x1f/path/icon.png\nItem2\n".to_vec(),
            items: vec![
                ResolvedItem::Program(ComputedProgram {
                    command: vec!["cmd1".to_string()],
                }),
                ResolvedItem::Program(ComputedProgram {
                    command: vec!["cmd2".to_string()],
                }),
            ],
        };

        let mut escaped_items = Vec::new();
        let escaped_flattened = flatten_resolved_menu(&escaped_resolved, &mut escaped_items);

        // Test the escaped input format
        let expected_input_escaped = escaped_resolved.input.escape_ascii().to_string();
        let actual_input_escaped = escaped_flattened.input.escape_ascii().to_string();
        assert_eq!(actual_input_escaped, expected_input_escaped);
        assert_eq!(
            actual_input_escaped,
            "Item1\\x00icon\\x1f/path/icon.png\\nItem2\\n"
        );
    }
}
