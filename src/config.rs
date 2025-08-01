use anyhow::{Context, Result, bail, ensure};
use bitcode::{Decode, Encode};
use kdl::{KdlDocument, KdlNode};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    ffi::OsStr,
    io::Write,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

pub fn default_config_path() -> PathBuf {
    let mut path;
    if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
        path = PathBuf::from(config_home);
    } else {
        path = std::env::home_dir().unwrap();
        path.push(".config");
    }
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

#[derive(Encode, Decode, Debug)]
pub struct ComputedProgram {
    pub command: Vec<String>,
}

struct InheritanceFrame {
    icon_dirs: Vec<PathBuf>,
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

        Self { icon_dirs }
    }
}

pub fn get_computed_config(path: &Path) -> Result<ComputedConfig> {
    let config_string = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    let actual_hash = Sha256::digest(&config_string);

    let cache_path = make_cache_path(path);
    let maybe_cached_config = read_cached_config(&cache_path);
    if let Some(cached_config) = maybe_cached_config
        && cached_config.hash == actual_hash[..8]
    {
        return Ok(cached_config);
    }

    let computed_config = compute_config(&config_string, actual_hash.as_slice())?;
    cache_config(&cache_path, &computed_config);
    Ok(computed_config)
}

fn make_cache_path(path: &Path) -> PathBuf {
    let mut cache_path = get_cache_dir();
    cache_path.push(path.file_stem().unwrap());
    cache_path.set_extension("cache");
    cache_path
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

fn compute_config(config_string: &str, hash: &[u8]) -> Result<ComputedConfig> {
    let config = parse_config(config_string)?;
    let mut items = Vec::new();
    let mut inheritance_stack = vec![InheritanceFrame::default()];
    let mut initial_menu = compute_menu_shallow(&config, &inheritance_stack);
    initial_menu.items_offset = 0;
    compute_menu_deep(&config, &mut items, &mut inheritance_stack);

    Ok(ComputedConfig {
        hash: std::array::from_fn(|i| hash[i]),
        initial_menu,
        items,
    })
}

fn compute_item_shallow(item: &Item, inheritance_stack: &[InheritanceFrame]) -> ComputedItem {
    match item.contents {
        ItemContents::Menu(ref menu) => {
            ComputedItem::Menu(compute_menu_shallow(menu, inheritance_stack))
        }
        ItemContents::Program(ref program) => ComputedItem::Program(ComputedProgram {
            command: program.command.clone(),
        }),
    }
}

fn search_for_icon(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
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

fn compute_menu_shallow(menu: &Menu, inheritance_stack: &[InheritanceFrame]) -> ComputedMenu {
    let args = menu.fuzzel_args.clone();
    // TODO: insert arguments for per-menu config

    let icon_dirs: Vec<PathBuf> = menu
        .icon_dirs
        .iter()
        .cloned()
        .chain(
            inheritance_stack
                .iter()
                .rev()
                .flat_map(|frame| frame.icon_dirs.clone()),
        )
        .collect();

    // format: {NAME}\0icon\x1f{ICON_PATH}\n
    let mut input = Vec::new();
    for item in &menu.items {
        write!(&mut input, "{}", item.name).unwrap();
        if let Some(icon) = &item.icon
            && let Some(icon_path) = search_for_icon(icon, &icon_dirs)
        {
            write!(&mut input, "\0icon\x1f{}", icon_path.display()).unwrap();
        }
        // TODO: Log an error if this failed
        writeln!(&mut input).unwrap();
    }

    ComputedMenu {
        args,
        input,
        items_offset: 0,
    }
}

fn compute_menu_deep(
    menu: &Menu,
    items: &mut Vec<ComputedItem>,
    inheritance_stack: &mut Vec<InheritanceFrame>,
) {
    inheritance_stack.push(InheritanceFrame {
        icon_dirs: menu.icon_dirs.clone(),
    });

    let mut current_index = items.len();

    for item in &menu.items {
        items.push(compute_item_shallow(item, inheritance_stack));
    }

    for item in &menu.items {
        if let ItemContents::Menu(menu) = &item.contents {
            let offset = items.len();
            if let &mut ComputedItem::Menu(ref mut computed_menu) = &mut items[current_index] {
                computed_menu.items_offset = offset;
            }
            compute_menu_deep(menu, items, inheritance_stack);
        }
        current_index += 1;
    }

    inheritance_stack.pop();
}

#[derive(Debug)]
struct Menu {
    fuzzel_args: Vec<String>,
    fuzzel_config: HashMap<String, String>,
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
    let mut fuzzel_config = HashMap::new();
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
                    fuzzel_config.insert(key, value);
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
                let home = std::env::home_dir().context("unable to get home directory")?;
                let path_str = node.entries()[0].value().as_string().unwrap().replace(
                    '~',
                    home.to_str()
                        .context("home path contains non-utf8 characters")?,
                );
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
