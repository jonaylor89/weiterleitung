use std::fmt;

/// A syntactically valid, normalised email address.
///
/// Normalisation lowercases the domain but preserves the case of the local
/// part, which is significant per RFC 5321 even though most hosts ignore it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmailAddress {
    local_part: String,
    domain: String,
}

impl EmailAddress {
    pub fn parse(s: impl AsRef<str>) -> Result<Self, String> {
        let s = s
            .as_ref()
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');

        let Some((local_part, domain)) = s.rsplit_once('@') else {
            return Err(format!("`{s}` is not a valid email address"));
        };

        let domain = domain.trim_end_matches('.').to_lowercase();

        let valid_local = !local_part.is_empty()
            && local_part.len() <= 64
            && !local_part.contains(char::is_whitespace)
            && !local_part.contains(['<', '>', ',', ';', '"', '\\']);
        let valid_domain = !domain.is_empty()
            && domain.len() <= 255
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.contains("..")
            && domain
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');

        if !valid_local || !valid_domain {
            return Err(format!("`{s}` is not a valid email address"));
        }

        Ok(Self {
            local_part: local_part.to_string(),
            domain,
        })
    }

    pub fn local_part(&self) -> &str {
        &self.local_part
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Lowercased form used for lookups and uniqueness.
    pub fn normalised(&self) -> String {
        format!("{}@{}", self.local_part.to_lowercase(), self.domain)
    }
}

impl fmt::Display for EmailAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.local_part, self.domain)
    }
}

impl TryFrom<String> for EmailAddress {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

#[cfg(test)]
mod tests {
    use super::EmailAddress;

    #[test]
    fn valid_addresses_are_parsed() {
        let address = EmailAddress::parse("Store.Name+tag@Example.COM").unwrap();
        assert_eq!(address.local_part(), "Store.Name+tag");
        assert_eq!(address.domain(), "example.com");
        assert_eq!(address.normalised(), "store.name+tag@example.com");
    }

    #[test]
    fn angle_brackets_and_trailing_dots_are_stripped() {
        let address = EmailAddress::parse("<alias@example.com.>").unwrap();
        assert_eq!(address.to_string(), "alias@example.com");
    }

    #[test]
    fn invalid_addresses_are_rejected() {
        for candidate in [
            "",
            "@example.com",
            "alias@",
            "alias@localhost",
            "alias example@example.com",
            "alias@exa mple.com",
            "alias@example..com",
        ] {
            assert!(
                EmailAddress::parse(candidate).is_err(),
                "`{candidate}` should be rejected"
            );
        }
    }
}
