//! Simple QR-like visual representation of wallet addresses.
//!
//! This does NOT produce a scannable QR code. It generates a visual
//! grid of Unicode block characters derived from the hex address,
//! useful for quick visual identification.

/// Generates a visual grid representation of a wallet address using
/// Unicode block characters.
///
/// Each hex character maps to a specific block pattern, producing a
/// recognizable visual fingerprint for the address.
pub fn address_to_qr_string(address: &str) -> String {
    // Strip optional "0x" prefix
    let hex_str = address.strip_prefix("0x").unwrap_or(address);

    // We'll create a square-ish grid from the hex characters.
    // Each hex nibble (0-15) maps to a pair of block characters.
    let block_chars: [&str; 16] = [
        "  ", // 0
        "\u{2588}\u{2588}", // 1  ██
        "\u{2584}\u{2584}", // 2  ▄▄
        "\u{2580}\u{2580}", // 3  ▀▀
        "\u{2588} ", // 4  █
        " \u{2588}", // 5   █
        "\u{2584}\u{2580}", // 6  ▄▀
        "\u{2580}\u{2584}", // 7  ▀▄
        "\u{2591}\u{2591}", // 8  ░░
        "\u{2592}\u{2592}", // 9  ▒▒
        "\u{2593}\u{2593}", // A  ▓▓
        "\u{2588}\u{2584}", // B  █▄
        "\u{2584}\u{2588}", // C  ▄█
        "\u{2580}\u{2588}", // D  ▀█
        "\u{2588}\u{2580}", // E  █▀
        "\u{2593}\u{2588}", // F  ▓█
    ];

    // Parse hex nibbles
    let nibbles: Vec<u8> = hex_str
        .chars()
        .filter_map(|c| c.to_digit(16).map(|d| d as u8))
        .collect();

    if nibbles.is_empty() {
        return String::from("(empty address)");
    }

    // Calculate grid dimensions (aim for roughly square)
    let total = nibbles.len();
    let width = (total as f64).sqrt().ceil() as usize;
    let width = width.max(4); // minimum 4 columns

    let mut result = String::new();

    // Top border
    result.push('\u{250c}');
    for _ in 0..width {
        result.push_str("\u{2500}\u{2500}");
    }
    result.push_str("\u{2510}\n");

    // Grid rows
    for row_start in (0..total).step_by(width) {
        result.push('\u{2502}');
        for col in 0..width {
            let idx = row_start + col;
            if idx < total {
                result.push_str(block_chars[nibbles[idx] as usize]);
            } else {
                result.push_str("  ");
            }
        }
        result.push_str("\u{2502}\n");
    }

    // Bottom border
    result.push('\u{2514}');
    for _ in 0..width {
        result.push_str("\u{2500}\u{2500}");
    }
    result.push('\u{2518}');

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_non_empty_output() {
        let output = address_to_qr_string("0xdeadbeef1234567890abcdef");
        assert!(!output.is_empty());
        assert!(output.contains('\u{2502}')); // contains vertical border
    }

    #[test]
    fn handles_address_without_prefix() {
        let with_prefix = address_to_qr_string("0xabcdef");
        let without_prefix = address_to_qr_string("abcdef");
        assert_eq!(with_prefix, without_prefix);
    }

    #[test]
    fn handles_empty_address() {
        let output = address_to_qr_string("");
        assert_eq!(output, "(empty address)");
    }

    #[test]
    fn output_has_borders() {
        let output = address_to_qr_string("0x1234abcd");
        // Should have top-left corner
        assert!(output.starts_with('\u{250c}'));
        // Should have bottom-right corner
        assert!(output.ends_with('\u{2518}'));
    }

    #[test]
    fn deterministic_output() {
        let a = address_to_qr_string("0xdeadbeef");
        let b = address_to_qr_string("0xdeadbeef");
        assert_eq!(a, b);
    }
}
