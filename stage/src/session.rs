//! Chat sessions — one conversation per session, persisted like cat's
//! per-session chat + a session log. Each session is a JSONL file under
//! ~/.pocket-cat/sessions/<id>.jsonl (one message per line); the directory
//! listing IS the session log. Switch, create, append; the widget renders
//! the current session inside the designed chatbox.

use std::fs;
use std::path::PathBuf;

pub struct Msg {
    pub role: String, // "user" | "pb"
    pub text: String,
}

pub struct Session {
    pub id: String,
    pub msgs: Vec<Msg>,
}

impl Session {
    /// A short title from the first user line (for the session log).
    pub fn title(&self) -> String {
        for m in &self.msgs {
            if m.role == "user" && !m.text.trim().is_empty() {
                return m.text.chars().take(18).collect();
            }
        }
        "New chat".to_string()
    }
}

pub struct Sessions {
    pub list: Vec<Session>,
    pub cur: usize,
    dir: PathBuf,
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

impl Sessions {
    pub fn load() -> Sessions {
        let home = std::env::var("HOME").unwrap_or_default();
        let dir = PathBuf::from(format!("{home}/.pocket-cat/sessions"));
        let _ = fs::create_dir_all(&dir);
        let mut list: Vec<Session> = Vec::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            let mut files: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
                .collect();
            files.sort(); // ids are timestamps → chronological
            for path in files {
                let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let mut msgs = Vec::new();
                if let Ok(text) = fs::read_to_string(&path) {
                    for line in text.lines() {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                            msgs.push(Msg {
                                role: v["role"].as_str().unwrap_or("pb").to_string(),
                                text: v["text"].as_str().unwrap_or("").to_string(),
                            });
                        }
                    }
                }
                list.push(Session { id, msgs });
            }
        }
        let mut s = Sessions { list, cur: 0, dir };
        if s.list.is_empty() {
            s.new_session();
        } else {
            s.cur = s.list.len() - 1; // resume the latest
        }
        s
    }

    pub fn current(&self) -> &Session {
        &self.list[self.cur]
    }

    pub fn new_session(&mut self) {
        let id = format!("{}", now_ms());
        // touch the file so it shows in the log even while empty
        let _ = fs::write(self.dir.join(format!("{id}.jsonl")), "");
        self.list.push(Session { id, msgs: Vec::new() });
        self.cur = self.list.len() - 1;
    }

    pub fn next(&mut self) {
        if self.cur + 1 < self.list.len() {
            self.cur += 1;
        }
    }
    pub fn prev(&mut self) {
        if self.cur > 0 {
            self.cur -= 1;
        }
    }

    pub fn append(&mut self, role: &str, text: &str) {
        let s = &mut self.list[self.cur];
        let rec = serde_json::json!({ "role": role, "text": text, "ts": now_ms() });
        use std::io::Write;
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(format!("{}.jsonl", s.id)))
        {
            let _ = f.write_all(format!("{}\n", rec).as_bytes());
        }
        s.msgs.push(Msg { role: role.to_string(), text: text.to_string() });
    }

    pub fn cur_id(&self) -> &str {
        &self.list[self.cur].id
    }
    pub fn pos_label(&self) -> String {
        format!("{}/{}", self.cur + 1, self.list.len())
    }
}
