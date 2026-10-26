//! Turns `on_open_urls` strings into Tidy's quick actions (`tidy://checkup` and
//! `tidy://check-for-updates`, listed in Info.plist as `SDShelfActions`). Only these two hosts
//! are acted on, with no path, query or fragment. Everything else is ignored and logged without
//! the URL's query text. No action takes a parameter, deletes or cleans anything.

/// A quick action Tidy answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// `tidy://checkup`: select Check up and run it (read-only; it never cleans).
    Checkup,
    /// `tidy://check-for-updates`: the same path as the Check for Updates menu item.
    CheckForUpdates,
}

/// Why a URL was not taken. Carries no part of the URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not in Tidy's scheme.
    NotTidy,
    /// A host that is not on the allowlist, or a path, query or fragment on an action.
    NotAllowed,
}

/// Reads one `tidy://<action>` URL (one trailing slash allowed, scheme and host in any case).
pub fn parse_action(s: &str) -> Result<Action, Refused> {
    let s = s.trim();
    let Some(rest) = s
        .get(.."tidy://".len())
        .filter(|h| h.eq_ignore_ascii_case("tidy://"))
        .map(|_| &s["tidy://".len()..])
    else {
        return Err(Refused::NotTidy);
    };
    let host = rest.strip_suffix('/').unwrap_or(rest);
    if host.eq_ignore_ascii_case("checkup") {
        Ok(Action::Checkup)
    } else if host.eq_ignore_ascii_case("check-for-updates") {
        Ok(Action::CheckForUpdates)
    } else {
        Err(Refused::NotAllowed)
    }
}

/// The actions in a batch of opened URLs, in order. Refused URLs are logged by kind only.
pub fn actions_for_open_urls(urls: &[String]) -> Vec<Action> {
    urls.iter()
        .filter_map(|u| match parse_action(u) {
            Ok(a) => Some(a),
            Err(why) => {
                tracing::warn!("open url ignored: {why:?}");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allowlist_is_exactly_two_hosts() {
        assert_eq!(parse_action("tidy://checkup"), Ok(Action::Checkup));
        assert_eq!(parse_action("tidy://checkup/"), Ok(Action::Checkup));
        assert_eq!(parse_action("TIDY://CheckUp"), Ok(Action::Checkup));
        assert_eq!(
            parse_action(" tidy://check-for-updates "),
            Ok(Action::CheckForUpdates)
        );
    }

    #[test]
    fn everything_else_is_refused() {
        for bad in [
            "tidy://checkup?clean=1",
            "tidy://checkup#x",
            "tidy://checkup/extra",
            "tidy://checkup//",
            "tidy://check-for-updates?now=1",
            "tidy://check-for-updates/x",
            "tidy://clean",
            "tidy://cleanup",
            "tidy://",
            "tidy:///checkup",
            "tidy://checkupp",
        ] {
            assert_eq!(parse_action(bad), Err(Refused::NotAllowed), "{bad}");
        }
        for other in [
            "tidy:checkup",
            "haul://paste",
            "https://example.com/checkup",
            "file:///tmp/a.torrent",
            "",
        ] {
            assert_eq!(parse_action(other), Err(Refused::NotTidy), "{other}");
        }
    }

    #[test]
    fn a_batch_keeps_the_allowed_ones_in_order() {
        let got = actions_for_open_urls(&[
            "tidy://check-for-updates".into(),
            "tidy://clean?all=1".into(),
            "tidy://checkup".into(),
        ]);
        assert_eq!(got, vec![Action::CheckForUpdates, Action::Checkup]);
    }

    /// The bundle's Info.plist claims `tidy:` (scripts/bundle.sh deletes the entry for other
    /// bundle ids) and lists the Shelf actions.
    #[test]
    fn info_plist_declares_the_scheme_and_the_shelf_actions() {
        let plist = include_str!("../../../resources/Info.plist.in");
        let urls = plist
            .find("<key>CFBundleURLTypes</key>")
            .expect("URL types");
        assert!(plist[urls..].contains("<array><string>tidy</string></array>"));
        let key = plist
            .find("<key>SDShelfActions</key>")
            .expect("SDShelfActions");
        let rest = &plist[key..];
        let array = &rest[..rest.find("</array>").expect("array end")];
        assert!(array.contains("<string>checkup</string>"));
        assert!(array.contains("<string>check-for-updates</string>"));
        assert_eq!(array.matches("<string>").count(), 2);
    }
}
