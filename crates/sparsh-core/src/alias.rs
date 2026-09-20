use std::collections::{BTreeMap, HashSet};

const MAX_ALIAS_DEPTH: usize = 64;

#[derive(Debug, Clone, Default)]
pub(crate) struct AliasService {
    entries: BTreeMap<String, Vec<String>>,
}

impl AliasService {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn get(&self, name: &str) -> Option<&[String]> {
        self.entries.get(name).map(Vec::as_slice)
    }

    pub(crate) fn define(&mut self, name: &str, expansion: Vec<String>) -> Result<(), String> {
        self.define_many(&[(name.to_string(), expansion)])
    }

    pub(crate) fn define_many(
        &mut self,
        definitions: &[(String, Vec<String>)],
    ) -> Result<(), String> {
        for (name, expansion) in definitions {
            validate_name(name)?;
            if expansion.is_empty() {
                return Err(format!("alias `{name}` requires a command"));
            }
        }
        for (name, expansion) in definitions {
            self.entries.insert(name.clone(), expansion.clone());
        }
        Ok(())
    }

    pub(crate) fn remove_many(&mut self, names: &[String]) -> Result<(), String> {
        for name in names {
            validate_name(name)?;
            if !self.entries.contains_key(name) {
                return Err(format!("alias not found: `{name}`"));
            }
        }
        for name in names {
            self.entries.remove(name);
        }
        Ok(())
    }

    pub(crate) fn expand(&self, program: &str, args: &[String]) -> Result<Vec<String>, String> {
        let mut words = std::iter::once(program.to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>();
        let mut visited = HashSet::new();
        let mut chain = Vec::new();

        for _ in 0..MAX_ALIAS_DEPTH {
            let Some(expansion) = self.entries.get(&words[0]) else {
                return Ok(words);
            };
            let name = words[0].clone();
            if !visited.insert(name.clone()) {
                chain.push(name);
                return Err(format!("alias cycle: {}", chain.join(" -> ")));
            }
            chain.push(name.clone());
            let mut expanded = expansion.clone();
            expanded.extend(words.into_iter().skip(1));

            // Shell aliases commonly wrap the real executable with the same
            // command name, e.g. `ls -> ls --color=auto`. Once an alias
            // expands to itself as the first word, that first word denotes
            // the underlying command and must not be expanded again. True
            // multi-alias cycles still revisit a previously expanded name on
            // a later iteration and are rejected by `visited` above.
            if expanded.first().is_some_and(|word| word == &name) {
                return Ok(expanded);
            }
            words = expanded;
        }

        Err(format!(
            "alias expansion exceeded {MAX_ALIAS_DEPTH} steps: {}",
            chain.join(" -> ")
        ))
    }

    pub(crate) fn render(&self) -> String {
        let mut output = String::new();
        for (name, expansion) in &self.entries {
            output.push_str("alias ");
            output.push_str(name);
            output.push_str(" = ");
            output.push_str(
                &expansion
                    .iter()
                    .map(|word| quote_word(word))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            output.push('\n');
        }
        output
    }

    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'=' | b'\0'))
    {
        return Err(format!("invalid alias name: `{name}`"));
    }
    Ok(())
}

fn quote_word(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./:@%+=,-".contains(&byte))
    {
        return word.to_string();
    }
    format!("'{}'", word.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use super::AliasService;

    #[test]
    fn expands_chained_prefixes_without_reparsing_text() {
        let mut aliases = AliasService::new();
        aliases.define("g", vec!["git".into()]).unwrap();
        aliases
            .define("gs", vec!["g".into(), "status".into()])
            .unwrap();

        let expanded = aliases.expand("gs", &["--short".into()]).unwrap();

        assert_eq!(expanded, ["git", "status", "--short"]);
    }

    #[test]
    fn self_prefixing_alias_wraps_real_command_without_cycle() {
        let mut aliases = AliasService::new();
        aliases
            .define("ls", vec!["ls".into(), "--color=auto".into()])
            .unwrap();

        let expanded = aliases.expand("ls", &["src".into()]).unwrap();

        assert_eq!(expanded, ["ls", "--color=auto", "src"]);
    }

    #[test]
    fn reports_alias_cycle_chain() {
        let mut aliases = AliasService::new();
        aliases.define("a", vec!["b".into()]).unwrap();
        aliases.define("b", vec!["a".into()]).unwrap();

        let error = aliases.expand("a", &[]).unwrap_err();

        assert!(error.contains("a -> b -> a"), "{error}");
    }

    #[test]
    fn definitions_validate_before_mutating() {
        let mut aliases = AliasService::new();
        aliases.define("keep", vec!["true".into()]).unwrap();

        let error = aliases
            .define_many(&[
                ("keep".into(), vec!["false".into()]),
                ("bad/name".into(), vec!["true".into()]),
            ])
            .unwrap_err();

        assert!(error.contains("bad/name"));
        assert_eq!(aliases.get("keep").unwrap(), ["true"]);
    }

    #[test]
    fn listing_is_stable_and_quotes_ambiguous_arguments() {
        let mut aliases = AliasService::new();
        aliases
            .define("z", vec!["printf".into(), "hello world".into()])
            .unwrap();
        aliases
            .define("a", vec!["git".into(), "status".into()])
            .unwrap();

        assert_eq!(
            aliases.render(),
            "alias a = git status\nalias z = printf 'hello world'\n"
        );
    }
}
