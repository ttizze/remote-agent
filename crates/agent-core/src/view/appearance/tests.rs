use super::*;
use crate::presentation::color::contrast_ratio;

fn hex(value: &str) -> String {
    to_hex(value)[..7].to_owned()
}

fn ratio(first: &str, second: &str) -> f64 {
    contrast_ratio(Color::parse(first).unwrap(), Color::parse(second).unwrap())
}

#[test]
fn boosts_semantic_contrast_above_the_default() {
    assert_eq!(
        contrast_mix(135),
        ContrastMix {
            base: 100.,
            boost: 35.,
            border_boost: 8.75
        }
    );
}

#[test]
fn supports_the_maximum_contrast_boost() {
    assert_eq!(
        contrast_mix(200),
        ContrastMix {
            base: 100.,
            boost: 100.,
            border_boost: 25.
        }
    );
}

#[test]
fn softens_semantic_contrast_below_the_default() {
    assert_eq!(
        contrast_mix(70),
        ContrastMix {
            base: 70.,
            boost: 0.,
            border_boost: 0.
        }
    );
}

#[test]
fn disables_contrast_mixing_at_the_default() {
    assert_eq!(
        contrast_mix(100),
        ContrastMix {
            base: 100.,
            boost: 0.,
            border_boost: 0.
        }
    );
    let appearance = Appearance::default();
    assert_eq!(appearance.palette(true).colors, theme(true).colors);
}

#[test]
fn contrast_moves_text_away_from_its_surface_and_fades_borders() {
    let stock = theme(false);
    let boosted = Appearance {
        contrast: 200,
        ..Appearance::default()
    }
    .palette(false);
    // At the maximum boost text reaches the target: black in light mode.
    assert_eq!(boosted.colors["textMuted"], "#000000");
    assert_ne!(boosted.colors["border"], stock.colors["border"]);
    let softened = Appearance {
        contrast: 50,
        ..Appearance::default()
    }
    .palette(false);
    assert!(
        ratio(&softened.colors["text"], &stock.colors["canvas"])
            < ratio(&stock.colors["text"], &stock.colors["canvas"])
    );
    // Borders keep their color at half opacity.
    assert_eq!(
        softened.colors["border"],
        format!("{}80", stock.colors["border"])
    );
}

#[test]
fn keeps_the_chat_palette_faithful_and_readable() {
    let chat = built_in_theme("chat").unwrap();
    for (dark, expected) in [
        (
            false,
            &[
                ("canvas", "#fdf7fd"),
                ("chrome", "#fdf7fd"),
                ("toolbarBorder", "#efbdeb"),
                ("toolbarControl", "#f3e6f5"),
                ("toolbarControlHover", "#eccfe3"),
                ("surfaceRaised", "#fdfafd"),
                ("input", "#e7c1dc"),
                ("focus", "#db2777"),
                ("messageSurface", "#f7def2"),
                ("codeBackground", "#f5ecf9"),
                ("codeForeground", "#673c8b"),
                ("accentSurface", "#f3e6f5"),
                ("sidebar", "#f2e1f4"),
            ][..],
        ),
        (
            true,
            &[
                ("canvas", "#1f1a24"),
                ("chrome", "#1f1a24"),
                ("surface", "#29232d"),
                ("surfaceRaised", "#2c2631"),
                ("input", "#302029"),
                ("focus", "#db2777"),
                ("messageSurface", "#2b2431"),
                ("codeBackground", "#1f1a24"),
                ("sidebar", "#171018"),
                ("sidebarBorder", "#322028"),
            ][..],
        ),
    ] {
        for (role, value) in expected {
            assert_eq!(hex(chat.color(dark, role).unwrap()), *value, "{role}");
        }
    }
    for dark in [false, true] {
        let color = |role| chat.color(dark, role).unwrap();
        assert!(ratio(color("text"), color("canvas")) >= 7.);
        assert!(ratio(color("textMuted"), color("canvas")) >= 4.5);
        assert!(ratio(color("messageForeground"), color("messageSurface")) >= 4.5);
        assert!(ratio(color("secondaryForeground"), color("secondary")) >= 4.5);
        assert!(ratio(color("sidebarForeground"), color("sidebar")) >= 4.5);
        assert!(ratio(color("accentForeground"), color("accent")) >= 4.5);
    }
}

#[test]
fn includes_the_dual_mode_built_in_themes() {
    let ids: Vec<&str> = BUILT_IN_THEMES.iter().map(|theme| theme.id).collect();
    assert_eq!(ids, ["chat", "grove", "ocean", "ember", "iris"]);
    for theme in BUILT_IN_THEMES {
        assert!(theme.color(false, "accent").unwrap().starts_with("oklch("));
        assert!(theme.color(true, "accent").unwrap().starts_with("oklch("));
        for dark in [false, true] {
            let color = |role| theme.color(dark, role).unwrap();
            assert!(ratio(color("text"), color("canvas")) >= 4.5, "{}", theme.id);
            let muted = ratio(color("textMuted"), color("canvas"));
            assert!(muted >= 4.5, "{}", theme.id);
            if theme.id != "chat" {
                assert!(muted < 5.5, "{}", theme.id);
                let expected = if dark { 5.082 } else { 4.705 };
                assert!((muted - expected).abs() < 0.05, "{} {muted}", theme.id);
            }
            for (foreground, background) in [
                ("accentForeground", "accent"),
                ("toolbarControlForeground", "toolbarControl"),
                ("messageForeground", "messageSurface"),
                ("messageActionForeground", "messageAction"),
                ("messageActionForeground", "messageActionHover"),
                ("mutedForeground", "muted"),
                ("placeholder", "surfaceRaised"),
            ] {
                assert!(
                    ratio(color(foreground), color(background)) >= 4.5,
                    "{} {foreground}",
                    theme.id
                );
            }
        }
        // Every role the stock palette names, the theme names too.
        for dark in [false, true] {
            assert_eq!(theme.colors(dark).len(), 57);
        }
    }
}

#[test]
fn a_theme_paints_both_appearances_over_the_stock_tokens() {
    let mut appearance = Appearance::default();
    appearance.use_theme(Some("grove"));
    let light = appearance.palette(false);
    let grove = built_in_theme("grove").unwrap();
    assert_eq!(
        light.colors["canvas"],
        to_hex(grove.color(false, "canvas").unwrap())
    );
    // Tokens outside the theme roles keep their stock values.
    assert_eq!(light.colors["statusSky"], theme(false).colors["statusSky"]);
    let dark = appearance.palette(true);
    assert_eq!(
        dark.colors["canvas"],
        to_hex(grove.color(true, "canvas").unwrap())
    );
}

#[test]
fn the_mode_picks_the_appearance_unless_it_follows_the_system() {
    let mut appearance = Appearance::default();
    assert!(appearance.resolved_dark(true));
    assert!(!appearance.resolved_dark(false));
    appearance.mode = AppearanceMode::Light;
    assert!(!appearance.resolved_dark(true));
    appearance.mode = AppearanceMode::Dark;
    assert!(appearance.resolved_dark(false));
}

#[test]
fn choosing_a_whole_theme_replaces_the_per_appearance_mix() {
    let mut appearance = Appearance::default();
    appearance.assign_theme(true, Some("ocean"));
    assert_eq!(appearance.theme_owner(true), Some("ocean"));
    assert_eq!(appearance.theme_owner(false), None);
    appearance.use_theme(Some("ember"));
    assert_eq!(appearance.theme_owner(true), Some("ember"));
    assert_eq!(appearance.theme_owner(false), Some("ember"));
    assert_eq!(appearance.dark_theme, None);
}

#[test]
fn picking_the_stock_palette_for_one_side_moves_the_theme_to_the_other() {
    let mut appearance = Appearance::default();
    appearance.use_theme(Some("iris"));
    appearance.assign_theme(true, None);
    assert_eq!(appearance.theme, None);
    assert_eq!(appearance.theme_owner(true), None);
    assert_eq!(appearance.theme_owner(false), Some("iris"));
    // A mixed pair keeps the other side's own theme.
    let mut appearance = Appearance::default();
    appearance.use_theme(Some("iris"));
    appearance.assign_theme(false, Some("grove"));
    appearance.assign_theme(false, None);
    assert_eq!(appearance.theme_owner(false), None);
    assert_eq!(appearance.theme_owner(true), Some("iris"));
}

#[test]
fn theme_cards_ring_the_appearances_each_one_paints() {
    let mut appearance = Appearance::default();
    let cards = appearance.theme_cards();
    assert_eq!(cards[0].label, STANDARD_THEME_LABEL);
    assert!(cards[0].light_owner && cards[0].dark_owner);
    assert_eq!(cards.len(), 1 + BUILT_IN_THEMES.len());
    appearance.assign_theme(true, Some("ocean"));
    let cards = appearance.theme_cards();
    let ocean = cards
        .iter()
        .find(|card| card.id.as_deref() == Some("ocean"))
        .unwrap();
    assert!(ocean.dark_owner && !ocean.light_owner);
    assert!(cards[0].light_owner && !cards[0].dark_owner);
    assert_eq!(appearance.wireframe_colors(true), ocean.previews[1].colors);
}

#[test]
fn the_preview_ball_pulls_the_canvas_toward_its_appearance() {
    let stock = preview_colors(None, false);
    assert_eq!(stock.canvas, "#fcfcfc");
    assert_eq!(stock.ball_base(false), "#fdfdfd");
    let dark = preview_colors(None, true);
    assert_eq!(dark.ball_base(true), "#0a0a0a");
}

#[test]
fn saved_preferences_round_trip_and_unreadable_ones_are_the_defaults() {
    let mut appearance = Appearance {
        contrast: 135,
        composer_context: true,
        prompt_font: "Menlo".into(),
        ..Appearance::default()
    };
    appearance.assign_theme(true, Some("grove"));
    assert_eq!(Appearance::decode(&appearance.encode()), appearance);
    assert_eq!(Appearance::decode(b"not json"), Appearance::default());
    assert_eq!(Appearance::decode(b""), Appearance::default());
}

#[test]
fn saved_values_are_pulled_back_into_their_bounds() {
    let saved = Appearance {
        contrast: 900,
        glass_opacity: 1,
        panel_animation_ms: 5000,
        interface_size: 2,
        terminal_size: 99,
        theme: Some("missing".into()),
        code_font: "x".repeat(MAX_FONT_FAMILY_CHARS + 1),
        ..Appearance::default()
    };
    let decoded = Appearance::decode(&saved.encode());
    assert_eq!(decoded.contrast, MAX_CONTRAST);
    assert_eq!(decoded.glass_opacity, MIN_GLASS_OPACITY);
    assert_eq!(decoded.panel_animation_ms, MAX_PANEL_ANIMATION_MS);
    assert_eq!(decoded.interface_size, INTERFACE_FONT_SIZES.0);
    assert_eq!(decoded.terminal_size, TERMINAL_FONT_SIZES.1);
    assert_eq!(decoded.theme, None);
    assert_eq!(decoded.code_font, "");
}

#[test]
fn simple_typography_runs_the_terminal_on_the_code_font() {
    let mut appearance = Appearance {
        code_font: "Fira Code".into(),
        code_size: 15,
        terminal_font: "Menlo".into(),
        terminal_size: 11,
        ..Appearance::default()
    };
    assert_eq!(appearance.terminal_font(), "Fira Code");
    assert_eq!(appearance.terminal_font_size(), 15);
    appearance.advanced_typography = true;
    assert_eq!(appearance.terminal_font(), "Menlo");
    assert_eq!(appearance.terminal_font_size(), 11);
}

#[test]
fn the_prompt_follows_the_interface_font_until_it_has_its_own() {
    let mut appearance = Appearance {
        interface_font: "Inter".into(),
        ..Appearance::default()
    };
    assert_eq!(appearance.prompt_font(), "Inter");
    appearance.prompt_font = "Menlo".into();
    assert_eq!(appearance.prompt_font(), "Menlo");
}

#[test]
fn mobile_appearance_normalizes_every_native_range() {
    let normalized = normalize_mobile_appearance(MobileAppearance {
        color_scheme: MobileColorScheme::Dark,
        theme: Some("missing".into()),
        light_theme: Some("chat".into()),
        dark_theme: Some("material-you".into()),
        base_font_size: 99,
        code_font_size: Some(1),
        terminal_font_size: Some(99.0),
        code_word_wrap: false,
    });
    assert_eq!(normalized.theme, None);
    assert_eq!(normalized.light_theme.as_deref(), Some("chat"));
    assert_eq!(normalized.dark_theme.as_deref(), Some("material-you"));
    assert_eq!(normalized.base_font_size, MOBILE_BASE_FONT_SIZES.1);
    assert_eq!(normalized.code_font_size, Some(MOBILE_CODE_FONT_SIZES.0));
    assert_eq!(normalized.terminal_font_size, Some(MAX_TERMINAL_FONT_SIZE));
    assert!(!normalized.code_word_wrap);
}

#[test]
fn mobile_theme_selection_can_make_stock_one_side_of_a_shared_theme() {
    let appearance = MobileAppearance {
        theme: Some("chat".into()),
        ..MobileAppearance::default()
    };
    let light_stock = mobile_assign_theme(appearance.clone(), false, None);
    assert_eq!(light_stock.theme, None);
    assert_eq!(light_stock.light_theme, None);
    assert_eq!(light_stock.dark_theme.as_deref(), Some("chat"));

    let dark_stock = mobile_assign_theme(appearance, true, None);
    assert_eq!(dark_stock.theme, None);
    assert_eq!(dark_stock.light_theme.as_deref(), Some("chat"));
    assert_eq!(dark_stock.dark_theme, None);
}

#[test]
fn mobile_typography_scales_body_and_uses_independent_code_and_terminal_choices() {
    let default = mobile_typography(MobileAppearance::default());
    assert_eq!(default.base_font_size, 16.0);
    assert_eq!(default.body_line_height, 23.0);
    assert_eq!(default.markdown_code_font_size, 13.0);
    assert_eq!(default.markdown_code_line_height, 19.0);
    assert_eq!(default.code_font_size, 12.0);
    assert_eq!(default.code_line_number_font_size, 11.0);
    assert_eq!(default.code_line_height, 22.0);
    assert_eq!(default.terminal_font_size, 10.5);

    let custom = mobile_typography(MobileAppearance {
        base_font_size: 20,
        code_font_size: Some(18),
        terminal_font_size: Some(6.5),
        ..MobileAppearance::default()
    });
    assert_eq!(custom.base_font_size, 20.0);
    assert_eq!(custom.body_line_height, 29.0);
    assert_eq!(custom.code_font_size, 18.0);
    assert_eq!(custom.code_line_number_font_size, 17.0);
    assert_eq!(custom.code_line_height, 33.0);
    assert_eq!(custom.terminal_font_size, 6.5);

    let scaled_default = mobile_typography(MobileAppearance {
        base_font_size: 22,
        ..MobileAppearance::default()
    });
    assert_eq!(scaled_default.markdown_body_font_size, 22.0);
    assert_eq!(scaled_default.markdown_body_line_height, 32.0);
    assert_eq!(scaled_default.markdown_h1_font_size, 29.0);
    assert_eq!(scaled_default.markdown_h2_font_size, 26.0);
    assert_eq!(scaled_default.markdown_h3_font_size, 23.0);
    assert_eq!(scaled_default.markdown_h4_font_size, 21.0);
    assert_eq!(scaled_default.markdown_code_font_size, 18.0);
    assert_eq!(scaled_default.code_font_size, 17.0);
    assert_eq!(scaled_default.code_line_height, 31.0);
    assert_eq!(scaled_default.terminal_font_size, MAX_TERMINAL_FONT_SIZE);
}
