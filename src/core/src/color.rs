//! ANSI color helpers for terminal output. No external crates.

const RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";

pub fn green(text: &str) -> String {
    format!("{}{}{}", GREEN, text, RESET)
}

pub fn yellow(text: &str) -> String {
    format!("{}{}{}", YELLOW, text, RESET)
}

pub fn red(text: &str) -> String {
    format!("{}{}{}", RED, text, RESET)
}

pub fn dim(text: &str) -> String {
    format!("{}{}{}", DIM, text, RESET)
}

pub fn bold(text: &str) -> String {
    format!("{}{}{}", BOLD, text, RESET)
}

pub fn cyan(text: &str) -> String {
    format!("{}{}{}", CYAN, text, RESET)
}

/// Check if stdout supports color (respects NO_COLOR convention).
pub fn supports_color() -> bool {
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    match std::env::var("TERM") {
        Ok(term) => term != "dumb",
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn green_wraps() {
        let result = green("hello");
        assert!(result.contains("\x1b[32m"));
        assert!(result.contains("hello"));
        assert!(result.ends_with(RESET));
    }

    #[test]
    fn red_wraps() {
        assert!(red("error").contains("\x1b[31m"));
    }

    #[test]
    fn yellow_wraps() {
        assert!(yellow("warn").contains("\x1b[33m"));
    }

    #[test]
    fn bold_wraps() {
        assert!(bold("important").contains("\x1b[1m"));
    }
}
