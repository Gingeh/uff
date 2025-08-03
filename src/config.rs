use anyhow::{Context, Result, bail, ensure};
use bitcode::{Decode, Encode};
use kdl::{KdlDocument, KdlNode};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    ffi::OsStr,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

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
        }

        let mut icon_dirs: Vec<PathBuf> = std::env::split_paths(&data_dirs).collect();

        let mut data_home = std::env::var("XDG_DATA_HOME").unwrap_or_default();
        if data_home.is_empty() {
            let home = std::env::home_dir().unwrap();
            data_home = format!("{}/.local/share/", home.display());
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
    if let Some(cached_config) = maybe_cached_config
        && cached_config.hash == actual_hash[..8]
    {
        return Ok(cached_config);
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

fn default_fuzzel_config_path() -> PathBuf {
    let mut path = default_config_dir();
    path.push("fuzzel");
    path.push("fuzzel.ini");
    path
}

fn create_fuzzel_config(
    pairs: &[(String, String)],
    id: usize,
    inherit_id: Option<usize>,
    preset_name: &str,
) -> PathBuf {
    let config_path = make_fuzzel_config_path(id, preset_name);
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

// TODO: Log errors
fn read_cached_config(path: &Path) -> Option<ComputedConfig> {
    let bytes = std::fs::read(path).ok()?;
    bitcode::decode(&bytes).ok()
}

// TODO: Log errors
fn cache_config(path: &Path, computed_config: &ComputedConfig) {
    let bytes = bitcode::encode(computed_config);
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(path, bytes);
}

fn compute_config(config_string: &str, hash: &[u8], preset_name: &str) -> Result<ComputedConfig> {
    let config = parse_config(config_string)?;
    let inheritance_stack = vec![InheritanceFrame::default()];

    // Build phase: create fully resolved tree with inheritance applied
    let resolved_menu = build_resolved_menu(&config, &inheritance_stack, 0, preset_name);

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
    id: usize,
    preset_name: &str,
) -> ResolvedMenu {
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
    for (item_index, item) in menu.items.iter().enumerate() {
        let item_id = id * 1000 + item_index + 1; // Generate unique IDs for nested items
        match &item.contents {
            ItemContents::Menu(child_menu) => {
                let mut child_inheritance_stack = inheritance_stack.to_vec();
                child_inheritance_stack.push(child_frame.clone());
                let resolved_child = build_resolved_menu(child_menu, &child_inheritance_stack, item_id, preset_name);
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

fn home() -> String {
    let home_path = std::env::home_dir().unwrap();
    home_path.to_string_lossy().to_string()
}

fn search_for_icon<'a>(name: &str, dirs: impl IntoIterator<Item = &'a Path>) -> Option<PathBuf> {
    if name.contains('/') {
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
    None
}

#[derive(Debug)]
struct Menu {
    fuzzel_args: Vec<String>,
    fuzzel_config: Vec<(String, String)>,
    icon_dirs: Vec<PathBuf>,
    items: Vec<Item>,
}

#[derive(Debug)]
struct Item {
    name: String,
    icon: Option<String>,
    contents: ItemContents,
}

#[derive(Debug)]
enum ItemContents {
    Menu(Menu),
    Program(Program),
}

#[derive(Debug)]
struct Program {
    command: Vec<String>,
}

fn parse_config(src: &str) -> Result<Menu> {
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
            "icon" => {} // already parsed by parse_item_from_nodes
            other => anyhow::bail!("unexpected node in menu: {}", other),
        }
    }

    Ok(Menu {
        fuzzel_args,
        fuzzel_config,
        icon_dirs,
        items,
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
