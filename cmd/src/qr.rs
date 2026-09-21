//! QR codes, rendered here as SVG rather than by a script in the browser —
//! the port of Park River's `shared/qr.rs`. An authenticator app is set up by
//! scanning one; typing a 32-character secret is the fallback, not the way.

/// An `otpauth://` URI as an inline SVG, sized in `viewBox` units so the page
/// decides how big it is. `crispEdges` keeps the modules square when small.
pub fn svg(data: &str) -> Result<String, String> {
    use qrcode::QrCode;
    let code = QrCode::new(data.as_bytes()).map_err(|e| format!("cannot encode a QR code: {e}"))?;
    let width = code.width();
    const QUIET: usize = 4;
    let side = width + QUIET * 2;
    let mut squares = String::new();
    for y in 0..width {
        for x in 0..width {
            if code[(x, y)] == qrcode::Color::Dark {
                squares.push_str(&format!(r#"<rect x="{}" y="{}" width="1" height="1"/>"#, x + QUIET, y + QUIET));
            }
        }
    }
    Ok(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {side} {side}" shape-rendering="crispEdges" role="img" aria-label="Two-factor setup QR code"><rect width="{side}" height="{side}" fill="#fff"/><g fill="#000">{squares}</g></svg>"##
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_otpauth_uri_becomes_an_svg() {
        let out = super::svg("otpauth://totp/Huntwell:a@b.c?secret=JBSWY3DPEHPK3PXP&issuer=Huntwell").unwrap();
        assert!(out.starts_with("<svg") && out.ends_with("</svg>") && out.contains("<rect"));
    }
}
