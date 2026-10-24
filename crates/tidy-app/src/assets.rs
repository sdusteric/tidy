//! Icons: the Lucide icons Tidy uses, layered over gpui-kit's own assets.

use std::borrow::Cow;

use gpui_kit::assets::icon_assets;
use gpui_kit::{AssetSource, Result, SharedString};

icon_assets!(
    pub TidyIcons,
    [
        Sparkles,
        Stethoscope,
        ShieldCheck,
        Clock,
        FolderSearch,
        Lock,
        Trash,
        Film,
        Archive,
        Disc,
        AppWindow,
        X
    ]
);

/// The app's asset source: Tidy's icons first, then gpui-kit's built-in assets.
/// Pass it to `gpui_kit::application().with_assets(AppAssets)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = TidyIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(TidyIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::assets::IconName;

    #[test]
    fn every_icon_the_screens_use_loads() {
        for icon in [
            // Tidy's own list.
            IconName::Sparkles,
            IconName::Clock,
            IconName::FolderSearch,
            IconName::Lock,
            IconName::Trash,
            IconName::Film,
            IconName::Archive,
            IconName::Disc,
            IconName::AppWindow,
            IconName::X,
            // From gpui-kit's default set.
            IconName::HardDrive,
            IconName::Ellipsis,
            IconName::RefreshCw,
            IconName::ChevronRight,
            IconName::ChevronDown,
            IconName::ChevronsUpDown,
            IconName::Check,
            IconName::Minus,
            IconName::Folder,
            IconName::FileText,
            IconName::LoaderCircle,
            IconName::TriangleAlert,
            IconName::Copy,
            IconName::CircleCheck,
            IconName::Inbox,
        ] {
            let path = icon.path();
            let bytes = AppAssets.load(&path).unwrap();
            assert!(bytes.is_some_and(|b| !b.is_empty()), "{path}");
        }
    }
}
