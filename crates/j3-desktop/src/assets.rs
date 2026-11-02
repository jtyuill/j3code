//! The app's icon assets: GPUI Kit's component icons plus the Lucide icons
//! the thread sidebars use (the same ones t3code's sidebar shows), the feed's
//! tool calls, the composer's usage gauge, and the model picker's provider
//! logos.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString, assets::Assets};

gpui_kit::assets::icon_assets!(
    SidebarIcons,
    [
        AlarmClock,
        AlarmClockOff,
        AppWindow,
        Archive,
        Bot,
        Braces,
        Brain,
        Bug,
        CircleDashed,
        CircleDotDashed,
        Clock,
        Code,
        FilePen,
        FilePlus,
        FileX,
        FolderPlus,
        FolderSearch,
        Gauge,
        Hourglass,
        ListFilter,
        ListTodo,
        Mail,
        MessageCircleQuestionMark,
        MessagesSquare,
        Pencil,
        Pin,
        PinOff,
        Plug,
        ScanSearch,
        SquarePen,
        Wrench,
    ]
);

/// [`SidebarIcons`] and the provider logos first, then GPUI Kit's [`Assets`].
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = SidebarIcons.load(path)? {
            return Ok(Some(bytes));
        }
        if let Some(bytes) = crate::logos::load(path) {
            return Ok(Some(bytes));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Assets.list(path)?;
        paths.extend(SidebarIcons.list(path)?);
        paths.extend(crate::logos::list(path).map(SharedString::from));
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
