#![feature(exit_status_error)]

use anyhow::{Context, Result, ensure};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

mod config;
use config::ComputedItem;

// TODO: Search for icons
// TODO: Create per-menu fuzzel configs in cache dir
// TODO: Tell fuzzel to use per-menu cache

pub fn main() -> Result<()> {
    let mut args = std::env::args();
    ensure!(args.len() < 3, "expected at most one argument");
    let config_path = args
        .nth(1)
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

        output
            .status
            .exit_ok()
            .context("fuzzel exited without success")?;

        let stdout = std::str::from_utf8(&output.stdout)?;
        let selected_index: usize = stdout.trim().parse()?;
        current_item = &computed_config.items[selected_index + current_menu.items_offset];
    }

    let ComputedItem::Program(program) = &current_item else {
        unreachable!();
    };

    Command::new(&program.command[0])
        .args(&program.command[1..])
        .spawn()
        .context("failed to spawn selected command")?;

    Ok(())
}
