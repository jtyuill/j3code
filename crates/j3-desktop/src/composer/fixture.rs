//! The `composer` scene's draft (`app::fixtures`): the new-thread composer focused with a
//! model chip, a magic keyword, a pasted image and omp's ghost text after the caret.

use super::draft;
use crate::{app::J3, prediction::Ghost};

/// A 1×1 PNG, the pasted image.
pub(super) const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// Fills the main composer's draft (call after the new-thread scene is installed).
pub fn install(s: &mut J3) {
    let Some(key) = s.main_composer_key() else {
        return;
    };
    let choices = s
        .envs
        .get(key.env())
        .map(|env| env.models().clone())
        .unwrap_or_default();
    s.add_composer_image(&key, PNG.to_vec(), Some("frame-times.png".to_owned()));
    let composer = s.composer(&key);
    let edit = &mut composer.edit;
    edit.replace_text(
        "Ask ^anthropic/claude-sonnet-4-5 to ultrathink about the anchor drift, then refac",
    );
    draft::sync_mentions(edit, &choices);
    edit.set_caret(edit.text.len());
    edit.focused = true;
    composer.ghost = Some(Ghost {
        text: edit.text.clone(),
        cursor: edit.focus,
        suffix: "tor".to_owned(),
    });
}
