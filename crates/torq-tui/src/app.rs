//! TUI state and key handling.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use torq_core::daemon::TorrentView;
use torq_sources::TorrentResult;

use crate::net::{Action, Client, UiMsg};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Search,
    Downloads,
}

pub struct App {
    pub tab: Tab,
    pub base: String,
    // Search tab
    pub input: String,
    pub searching: bool,
    pub results: Vec<TorrentResult>,
    pub offline: Vec<String>,
    pub search_selected: usize,
    // Downloads tab
    pub torrents: Vec<TorrentView>,
    pub dl_selected: usize,
    // Shared
    pub help: bool,
    pub notice: Option<String>,
}

impl App {
    pub fn new(base: String) -> Self {
        Self {
            tab: Tab::Search,
            base,
            input: String::new(),
            searching: false,
            results: Vec::new(),
            offline: Vec::new(),
            search_selected: 0,
            torrents: Vec::new(),
            dl_selected: 0,
            help: false,
            notice: None,
        }
    }

    pub fn apply(&mut self, msg: UiMsg) {
        match msg {
            UiMsg::Torrents(v) => {
                self.torrents = v;
                self.dl_selected = self.dl_selected.min(self.torrents.len().saturating_sub(1));
            }
            UiMsg::Search(Ok(report)) => {
                self.results = report.results;
                self.offline = report.offline;
                self.search_selected = 0;
                self.searching = false;
            }
            UiMsg::Search(Err(e)) => {
                self.notice = Some(e.to_string());
                self.searching = false;
            }
            UiMsg::Notice(s) => self.notice = Some(s),
        }
    }

    /// Handle a key; returns `None` when the user wants to quit.
    pub fn handle_key(&mut self, key: KeyEvent, client: &Client) -> Option<()> {
        if self.help {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('q' | '?') => {
                    self.help = false;
                    Some(())
                }
                _ => Some(()),
            };
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        match self.tab {
            Tab::Search => self.search_key(key, client),
            Tab::Downloads => self.downloads_key(key, client),
        }
    }

    fn search_key(&mut self, key: KeyEvent, client: &Client) -> Option<()> {
        match key.code {
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('q') => return None,
            KeyCode::Char('1') | KeyCode::Tab => self.tab = Tab::Search,
            KeyCode::Char('2') => self.tab = Tab::Downloads,
            KeyCode::Enter => {
                if !self.searching {
                    self.searching = true;
                    self.notice = None;
                    client.send(Action::Search(self.input.clone()));
                }
            }
            KeyCode::Esc => self.input.clear(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            // Letter shortcuts must precede the typing arm below.
            KeyCode::Char('d') => {
                if let Some(r) = self.results.get(self.search_selected) {
                    client.send(Action::Add {
                        magnet: r.magnet.clone(),
                    });
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.input.push(c);
            }
            KeyCode::Down => {
                self.search_selected = self
                    .search_selected
                    .saturating_add(1)
                    .min(self.results.len().saturating_sub(1))
            }
            KeyCode::Up => self.search_selected = self.search_selected.saturating_sub(1),
            _ => {}
        }
        Some(())
    }

    fn downloads_key(&mut self, key: KeyEvent, client: &Client) -> Option<()> {
        match key.code {
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('q') => return None,
            KeyCode::Char('1') => self.tab = Tab::Search,
            KeyCode::Char('2') | KeyCode::Tab => self.tab = Tab::Downloads,
            KeyCode::Down => {
                self.dl_selected = self
                    .dl_selected
                    .saturating_add(1)
                    .min(self.torrents.len().saturating_sub(1))
            }
            KeyCode::Up => self.dl_selected = self.dl_selected.saturating_sub(1),
            KeyCode::Char('r') => client.send(Action::Refresh),
            KeyCode::Char('p') => {
                if let Some(v) = self.torrents.get(self.dl_selected) {
                    match v.status {
                        torq_core::daemon::Status::Paused | torq_core::daemon::Status::Queued => {
                            client.send(Action::Resume(v.id))
                        }
                        _ => client.send(Action::Pause(v.id)),
                    }
                }
            }
            KeyCode::Char('x') | KeyCode::Char('D') => {
                if let Some(v) = self.torrents.get(self.dl_selected) {
                    let delete_files = key.code == KeyCode::Char('D');
                    client.send(Action::Remove {
                        id: v.id,
                        delete_files,
                    });
                }
            }
            _ => {}
        }
        Some(())
    }
}
