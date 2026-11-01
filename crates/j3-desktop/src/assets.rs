//! The app's icon assets: GPUI Kit's component icons plus the Lucide icons
//! the thread sidebars use (the same ones t3code's sidebar shows).

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString, assets::Assets};

gpui_kit::assets::icon_assets!(
    SidebarIcons,
    [
        AlarmClock,
        AlarmClockOff,
        Archive,
        CircleDashed,
        CircleDotDashed,
        Clock,
        ListFilter,
        Mail,
        MessageCircleQuestionMark,
        Pencil,
        Pin,
        PinOff,
        SquarePen,
    ]
);

/// [`SidebarIcons`] first, then GPUI Kit's [`Assets`].
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = SidebarIcons.load(path)? {
            return Ok(Some(bytes));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Assets.list(path)?;
        paths.extend(SidebarIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
