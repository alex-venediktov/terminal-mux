use std::path::Path;

use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SshHost {
    pub alias: String,
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<String>,
}

// Хосты из ~/.ssh/config: псевдонимы записей Host без шаблонов, с HostName, User и Port своего блока
pub fn list_hosts() -> Vec<SshHost> {
    dirs::home_dir()
        .map(|h| h.join(".ssh").join("config"))
        .filter(|p| p.is_file())
        .map(|p| parse_file(&p))
        .unwrap_or_default()
}

fn parse_file(path: &Path) -> Vec<SshHost> {
    std::fs::read_to_string(path).map(|t| parse(&t)).unwrap_or_default()
}

pub fn parse(text: &str) -> Vec<SshHost> {
    let mut hosts: Vec<SshHost> = Vec::new();
    // Индексы хостов текущего блока Host; блок Match в список не попадает
    let mut current: Vec<usize> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = split_option(line);
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                current.clear();
                for alias in value.split_whitespace() {
                    if alias.contains(['*', '?', '!']) || hosts.iter().any(|h| h.alias == alias) {
                        continue;
                    }
                    hosts.push(SshHost { alias: alias.to_string(), host_name: None, user: None, port: None });
                    current.push(hosts.len() - 1);
                }
            }
            "match" => current.clear(),
            "hostname" | "user" | "port" => {
                for &i in &current {
                    let slot = match key.to_ascii_lowercase().as_str() {
                        "hostname" => &mut hosts[i].host_name,
                        "user" => &mut hosts[i].user,
                        _ => &mut hosts[i].port,
                    };
                    if slot.is_none() {
                        *slot = Some(value.trim_matches('"').to_string());
                    }
                }
            }
            _ => {}
        }
    }
    hosts
}

// Ключ и значение строки конфига: разделитель - пробелы или "=" (ssh_config(5))
fn split_option(line: &str) -> (&str, &str) {
    let end = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    let rest = line[end..].trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    (&line[..end], rest.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Шаблоны пропускаются, псевдонимы одной строки получают параметры своего блока, Match не создает хостов.
    #[test]
    fn ssh_config_yields_concrete_hosts_with_block_options() {
        let text = "\
# комментарий
Host *
  User everyone
Host web web-alias
  HostName 10.0.0.5
  User=deploy
  Port 2222
Host db
  hostname db.local
Match host db
  User other
Host !bad gw?
  HostName x
";
        let hosts = parse(text);
        assert_eq!(hosts.iter().map(|h| h.alias.as_str()).collect::<Vec<_>>(), ["web", "web-alias", "db"]);
        assert_eq!(hosts[1].host_name.as_deref(), Some("10.0.0.5"));
        assert_eq!(hosts[1].user.as_deref(), Some("deploy"));
        assert_eq!(hosts[0].port.as_deref(), Some("2222"));
        assert_eq!(hosts[2].host_name.as_deref(), Some("db.local"));
        assert_eq!(hosts[2].user, None);
    }
}
