/// Shell-quote a value using POSIX single-quote escaping.
pub(crate) fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn wraps_plain_value() {
        assert_eq!(shell_quote("plain"), "'plain'");
    }

    #[test]
    fn escapes_single_quotes() {
        assert_eq!(shell_quote("O'Reilly"), "'O'\"'\"'Reilly'");
    }
}
