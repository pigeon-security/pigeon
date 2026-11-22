//! Turns `tokens.css` into the Swift source of the iOS app's design tokens. Pure: no I/O.

use std::collections::HashMap;

/// Tokens that are not design values and are left out of the Swift file.
const SKIPPED: [&str; 3] = ["color-scheme", "font-sans", "font-mono"];

/// The Swift source for the tokens in `css`: dark values from the first `:root` block, light
/// values from the `[data-theme="light"]` block. An unknown value shape is an error naming it.
pub fn tokens(css: &str) -> Result<String, String> {
    let css = strip_comments(css);
    let dark = declarations(&block(&css, ":root {")?)?;
    let light: HashMap<String, String> =
        declarations(&block(&css, ":root[data-theme=\"light\"] {")?)?
            .into_iter()
            .collect();

    let mut lines: Vec<String> = Vec::new();
    for (name, value) in &dark {
        if SKIPPED.contains(&name.as_str()) {
            continue;
        }
        let swift = camel(name);
        let bad = |what: &str| format!("token --{name}: {what}: {value}");
        if value.starts_with('#') || value.starts_with("rgba(") {
            let d = colour(value).map_err(|e| bad(&e))?;
            let l = light
                .get(name)
                .ok_or_else(|| format!("token --{name}: colour missing from the light theme"))?;
            let l = colour(l).map_err(|e| format!("token --{name} (light): {e}: {l}"))?;
            lines.push(format!(
                "    static let {swift} = dynamic(light: {l}, dark: {d})"
            ));
        } else if value.ends_with("px") {
            let n = length(value).map_err(|e| bad(&e))?;
            lines.push(format!("    static let {swift}: CGFloat = {n}"));
        } else if name.starts_with("type-") {
            let (weight, size, line_height, mono) = type_style(value).map_err(|e| bad(&e))?;
            lines.push(format!(
                "    static let {swift} = TypeToken(weight: {weight}, size: {size}, \
                 lineHeight: {line_height}, mono: {mono})"
            ));
        } else {
            return Err(bad("unknown value shape"));
        }
    }
    for name in light.keys() {
        if !SKIPPED.contains(&name.as_str()) && !dark.iter().any(|(n, _)| n == name) {
            return Err(format!("token --{name}: in the light theme only"));
        }
    }

    Ok(format!(
        "{HEADER}{}\n\n{DYNAMIC}}}\n",
        lines.join("\n"),
        HEADER = HEADER,
        DYNAMIC = DYNAMIC,
    ))
}

/// The Swift source for the tokens the web design system ships.
pub fn generated() -> Result<String, String> {
    tokens(super::TOKENS)
}

const HEADER: &str =
    "// Generated from src/core/src/design/tokens.css by pigeon_core::design::swift. Do not edit:
// change tokens.css, then regenerate (see ios/README.md). The smoke suite fails on drift.
import SwiftUI
import UIKit

/// A type style from the web design system.
struct TypeToken { let weight: Int; let size: CGFloat; let lineHeight: CGFloat; let mono: Bool }

/// pigeon's design tokens, light and dark, as the web design system defines them.
enum Tokens {
";

const DYNAMIC: &str = "    /// A colour that follows the light or dark appearance.
    private static func dynamic(light: (Int, Int, Int, Double), dark: (Int, Int, Int, Double)) -> Color {
        Color(UIColor { traits in
            let c = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(
                red: CGFloat(c.0) / 255, green: CGFloat(c.1) / 255,
                blue: CGFloat(c.2) / 255, alpha: CGFloat(c.3))
        })
    }
";

/// `css` without `/* ... */` comments.
fn strip_comments(css: &str) -> String {
    let mut out = String::new();
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        match rest[i..].find("*/") {
            Some(j) => rest = &rest[i + j + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The text between `open` and the next `}`.
fn block(css: &str, open: &str) -> Result<String, String> {
    let start = css
        .find(open)
        .ok_or_else(|| format!("no `{open}` block in tokens.css"))?
        + open.len();
    let len = css[start..]
        .find('}')
        .ok_or_else(|| format!("`{open}` block is not closed"))?;
    Ok(css[start..start + len].to_string())
}

/// The `--name: value;` pairs of a block, in order, names without the dashes.
fn declarations(body: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for part in body.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, value) = part
            .split_once(':')
            .ok_or_else(|| format!("not a declaration: {part}"))?;
        let name = name.trim();
        let name = name.strip_prefix("--").unwrap_or(name);
        out.push((name.to_string(), value.trim().to_string()));
    }
    Ok(out)
}

/// `kebab-case` as `lowerCamelCase`.
fn camel(name: &str) -> String {
    let mut out = String::new();
    for (i, word) in name.split('-').enumerate() {
        let mut chars = word.chars();
        match chars.next() {
            Some(c) if i > 0 => {
                out.push(c.to_ascii_uppercase());
                out.push_str(chars.as_str());
            }
            Some(c) => {
                out.push(c);
                out.push_str(chars.as_str());
            }
            None => {}
        }
    }
    out
}

/// A Swift tuple `(0xRR, 0xGG, 0xBB, alpha)` for `#RRGGBB` or `rgba(r, g, b, a)`.
fn colour(value: &str) -> Result<String, String> {
    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("not #RRGGBB".into());
        }
        let hex = hex.to_ascii_uppercase();
        return Ok(format!(
            "(0x{}, 0x{}, 0x{}, 1.0)",
            &hex[0..2],
            &hex[2..4],
            &hex[4..6]
        ));
    }
    let inner = value
        .strip_prefix("rgba(")
        .and_then(|v| v.strip_suffix(')'))
        .ok_or("not a colour")?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    let [r, g, b, a] = parts.as_slice() else {
        return Err("rgba needs four numbers".into());
    };
    let mut ch = [0u8; 3];
    for (slot, text) in ch.iter_mut().zip([r, g, b]) {
        *slot = text.parse().map_err(|_| "bad rgba channel".to_string())?;
    }
    let alpha: f64 = a.parse().map_err(|_| "bad rgba alpha".to_string())?;
    if !(0.0..=1.0).contains(&alpha) {
        return Err("rgba alpha out of range".into());
    }
    Ok(format!("({}, {}, {}, {:?})", ch[0], ch[1], ch[2], alpha))
}

/// The number of `NNpx` or `N.Npx`, as Swift text.
fn length(value: &str) -> Result<String, String> {
    let n = value.strip_suffix("px").ok_or("not a px length")?;
    n.parse::<f64>()
        .map_err(|_| "not a px length".to_string())?;
    if n.starts_with('-') || n.starts_with('+') || n.contains(['e', 'E']) {
        return Err("not a px length".into());
    }
    Ok(n.to_string())
}

/// `WEIGHT SIZEpx/LINEHEIGHT var(--font-sans|--font-mono)` as (weight, size, line height, mono).
fn type_style(value: &str) -> Result<(u32, String, String, bool), String> {
    let bad = || "not a type style".to_string();
    let parts: Vec<&str> = value.split_whitespace().collect();
    let [weight, size_line, font] = parts.as_slice() else {
        return Err(bad());
    };
    let weight: u32 = weight.parse().map_err(|_| bad())?;
    let (size, line_height) = size_line.split_once('/').ok_or_else(bad)?;
    let size = length(size).map_err(|_| bad())?;
    line_height.parse::<f64>().map_err(|_| bad())?;
    let mono = match *font {
        "var(--font-sans)" => false,
        "var(--font-mono)" => true,
        _ => return Err(bad()),
    };
    Ok((weight, size, line_height.to_string(), mono))
}
