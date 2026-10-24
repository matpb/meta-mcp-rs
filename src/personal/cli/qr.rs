//! Terminal rendering of the pairing QR code.

use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;

/// Two modules per character row, quiet zone on, light modules drawn as blocks so the code
/// reads right on a dark terminal.
pub fn render(code: &str) -> Result<String, qrcode::types::QrError> {
    let qr = QrCode::new(code.as_bytes())?;
    Ok(qr
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .quiet_zone(true)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_output_has_the_expected_rows_and_only_block_characters() {
        // "HELLO" is a version 1 code: 21 modules plus a 4 module quiet zone on each side.
        let out = render("HELLO").expect("render");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 15, "{out}");
        for line in &lines {
            assert_eq!(line.chars().count(), 29, "{line:?}");
            assert!(
                line.chars()
                    .all(|c| matches!(c, ' ' | '\u{2580}' | '\u{2584}' | '\u{2588}')),
                "{line:?}"
            );
        }
        assert!(lines[0].chars().all(|c| c == '\u{2588}'), "quiet zone row");
    }

    #[test]
    fn longer_payloads_render_bigger_codes() {
        let payload = format!("2@{},{},{}", "a".repeat(60), "b".repeat(44), "c".repeat(44));
        let out = render(&payload).expect("render");
        assert!(out.lines().count() > 15);
        assert!(
            out.lines()
                .all(|l| l.chars().count() == out.lines().next().expect("row").chars().count())
        );
    }
}
