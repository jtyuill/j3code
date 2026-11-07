//! Offline terminal content for the `terminal` scene and tests: a drawer with a shell that
//! printed a coloured prompt, `ls` and `git status`, and a second tab, without attaching
//! to a server.

use super::{Drawer, TerminalStatus};
use crate::{app::J3, sidebar::ThreadKey};

/// What the scene's shell printed: prompts, `ls --color`, `git status -sb` and a 256-colour
/// and true-colour line, ending at a prompt.
pub const OUTPUT: &str = concat!(
    "\x1b[1;32mjacob@workstation\x1b[0m:\x1b[1;34m~/repos/j3code\x1b[0m$ ls --color\r\n",
    "\x1b[1;34mcrates\x1b[0m  \x1b[1;34mdocs\x1b[0m  \x1b[1;32mflake.nix\x1b[0m  ",
    "Cargo.lock  Cargo.toml  README.md  \x1b[1;36mresult\x1b[0m  \x1b[1;34mscripts\x1b[0m  ",
    "\x1b[01;31mrelease.tar.gz\x1b[0m\r\n",
    "\x1b[1;32mjacob@workstation\x1b[0m:\x1b[1;34m~/repos/j3code\x1b[0m$ git status -sb\r\n",
    "## \x1b[32mrune-desktop\x1b[0m...\x1b[31morigin/rune-desktop\x1b[0m [ahead 2]\r\n",
    " \x1b[31mM\x1b[0m crates/j3-desktop/src/terminal/mod.rs\r\n",
    "\x1b[32mA\x1b[0m  crates/j3-desktop/src/terminal/render.rs\r\n",
    "\x1b[31m??\x1b[0m notes.txt\r\n",
    "\x1b[1;32mjacob@workstation\x1b[0m:\x1b[1;34m~/repos/j3code\x1b[0m$ echo ",
    "\x1b[38;5;208m256\x1b[0m \x1b[38;2;120;200;255mtrue\x1b[0m \x1b[7minverse\x1b[0m ",
    "\x1b[4munderline\x1b[0m \x1b[2mfaint\x1b[0m \x1b[3mitalic\x1b[0m\r\n",
    "\x1b[38;5;208m256\x1b[0m \x1b[38;2;120;200;255mtrue\x1b[0m \x1b[7minverse\x1b[0m ",
    "\x1b[4munderline\x1b[0m \x1b[2mfaint\x1b[0m \x1b[3mitalic\x1b[0m\r\n",
    "\x1b[1;32mjacob@workstation\x1b[0m:\x1b[1;34m~/repos/j3code\x1b[0m$ ",
);

/// Opens `key`'s drawer with two terminals: `term-1` running and showing [`OUTPUT`],
/// focused, and `term-2` not yet attached. Neither attaches: `term-1` counts as sized and
/// running, `term-2` is never laid out unless selected.
pub fn install(s: &mut J3, key: &ThreadKey) {
    s.terminals.insert(key.clone(), Drawer::default());
    s.add_terminal(key);
    s.add_terminal(key);
    let drawer = s.terminals.get_mut(key).expect("just inserted");
    drawer.open = true;
    drawer.active = 0;
    drawer.focus_active();
    let terminal = &mut drawer.tabs[0];
    terminal.sized = true;
    terminal.status = TerminalStatus::Running;
    terminal.screen.feed(OUTPUT.as_bytes());
}
