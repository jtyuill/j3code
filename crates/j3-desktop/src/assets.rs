//! The app's icon assets: GPUI Kit's component icons plus the Lucide icons
//! the app draws beyond them (the thread sidebars' — the same ones t3code's
//! sidebar shows —, the feed's tool calls, the file pane's file types, the
//! agents hub, the usage gauge, the dictation microphone) and the model
//! picker's provider logos.

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
        Circle,
        CircleDashed,
        CircleDotDashed,
        CircleSlash,
        Clock,
        Code,
        FileArchive,
        FileBraces,
        FileCode,
        FileCog,
        FileImage,
        FileKey,
        FileLock,
        FilePen,
        FilePlus,
        FileTerminal,
        FileX,
        FolderPlus,
        FolderSearch,
        FolderTree,
        Gauge,
        Hourglass,
        ListFilter,
        ListTodo,
        Mail,
        MessageCircleQuestionMark,
        MessagesSquare,
        Mic,
        Pencil,
        Pin,
        PinOff,
        Plug,
        ScanSearch,
        SquarePen,
        Wrench,
        X,
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
