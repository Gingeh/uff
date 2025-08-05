use anyhow::{Context, Result, ensure};
use colog::format::CologStyle;
use log::{Level, LevelFilter, info};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

mod config;
mod parser;
use config::ComputedItem;

struct LogFormatter;
impl CologStyle for LogFormatter {
    fn level_token(&self, level: &log::Level) -> &str {
        match *level {
            Level::Error => "E",
            Level::Warn => "W",
            Level::Info => "I",
            Level::Debug => "D",
            Level::Trace => "T",
        }
    }
}

pub fn main() -> Result<()> {
    colog::default_builder()
        .format(colog::formatter(LogFormatter))
        .filter_level(LevelFilter::Info)
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() > 2 || args.get(1) == Some(&"--help".into()) || args.get(1) == Some(&"-h".into())
    {
        println!("usage: {} [config_path]", args[0]);
        println!("config_path defaults to $XDG_CONFIG_HOME/uff/default.kdl");
        return Ok(());
    }

    let config_path = args
        .get(1)
        .map_or_else(config::default_config_path, PathBuf::from);

    let computed_config = config::get_computed_config(&config_path)?;

    let mut current_item = &ComputedItem::Menu(computed_config.initial_menu);
    while let ComputedItem::Menu(current_menu) = current_item {
        let mut fuzzel = Command::new("fuzzel")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .args(["--dmenu", "--index"])
            .args(&current_menu.args)
            .spawn()
            .context("failed to spawn fuzzel")?;

        let mut fuzzel_stdin = fuzzel
            .stdin
            .take()
            .context("failed to get fuzzel's stdin")?;

        fuzzel_stdin
            .write_all(&current_menu.input)
            .context("failed to pass input to fuzzel")?;

        drop(fuzzel_stdin); // fuzzel waits until stdin is closed

        let output = fuzzel
            .wait_with_output()
            .context("failed to wait on fuzzel")?;

        ensure!(output.status.success(), "fuzzel exited without success");

        let stdout = std::str::from_utf8(&output.stdout)?;
        let selected_index: usize = stdout.trim().parse()?;
        current_item = &computed_config.items[selected_index + current_menu.items_offset];
    }

    let ComputedItem::Program(program) = &current_item else {
        unreachable!();
    };

    info!("running program: {}", program.command.join(" "));
    Command::new(&program.command[0])
        .args(&program.command[1..])
        .spawn()
        .context("failed to spawn selected command")?;

    Ok(())
}
