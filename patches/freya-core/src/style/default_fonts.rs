use std::borrow::Cow;

pub fn default_fonts() -> Vec<Cow<'static, str>> {
    let (lead, emoji, drawing, tail): (&[&str], &[&str], &[&str], &[&str]) = if cfg!(target_os = "windows") {
        (
            &["Segoe UI"],
            &["Segoe UI Emoji"],
            &["Cascadia Mono", "Consolas"],
            &[
                "Segoe UI Symbol",
                "Microsoft YaHei UI",
                "Yu Gothic UI",
                "Malgun Gothic",
                "Microsoft JhengHei UI",
                "Nirmala UI",
                "Leelawadee UI",
                "Ebrima",
                "Gadugi",
                "Javanese Text",
                "Myanmar Text",
                "Mongolian Baiti",
                "Segoe UI Historic",
                "Cambria Math",
            ],
        )
    } else if cfg!(target_os = "macos") {
        (
            &[".AppleSystemUIFont"],
            &["Apple Color Emoji"],
            &["Menlo"],
            &[
                "Apple Symbols",
                "PingFang SC",
                "Hiragino Sans",
                "Apple SD Gothic Neo",
                "PingFang TC",
                "Kohinoor Devanagari",
                "Thonburi",
                "STIXGeneral",
            ],
        )
    } else if cfg!(target_os = "linux") {
        (
            &["Ubuntu", "Adwaita Sans"],
            &["Noto Color Emoji"],
            &["DejaVu Sans Mono", "Noto Sans Mono"],
            &[
                "Noto Sans Symbols",
                "Noto Sans Symbols 2",
                "Noto Sans Math",
                "Noto Sans CJK SC",
                "Noto Sans CJK JP",
                "Noto Sans CJK KR",
                "Noto Sans Arabic",
                "Noto Sans Hebrew",
                "Noto Sans Thai",
                "Noto Sans Devanagari",
                "Unifont",
            ],
        )
    } else if cfg!(target_os = "android") {
        (&["Roboto"], &[], &[], &[])
    } else {
        (&[], &[], &[], &[])
    };
    lead.iter()
        .copied()
        .chain(["Noto Sans", "DejaVu Sans"])
        .chain(emoji.iter().copied())
        .chain(drawing.iter().copied())
        .chain(["Arial"])
        .chain(tail.iter().copied())
        .map(Cow::Borrowed)
        .collect()
}

/// OS-specific monospace fonts.
pub fn default_monospace_fonts() -> Vec<Cow<'static, str>> {
    let mut fonts = vec!["Noto Sans Mono".into(), "Courier New".into()];
    if cfg!(target_os = "windows") {
        fonts.insert(0, "Cascadia Mono".into());
        fonts.insert(1, "Consolas".into());
    } else if cfg!(target_os = "macos") {
        fonts.insert(0, "Menlo".into());
    } else if cfg!(target_os = "linux") {
        fonts.insert(0, "Ubuntu Mono".into());
        fonts.insert(1, "Adwaita Mono".into());
        fonts.insert(2, "DejaVu Sans Mono".into());
    } else if cfg!(target_os = "android") {
        fonts.insert(0, "monospace".into());
    }
    fonts
}
