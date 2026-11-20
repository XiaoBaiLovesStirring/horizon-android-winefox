//! Agent control as a setting (Horizon Settings > AI Agent): when the control socket is open, who
//! is connected, and the details an agent needs. The socket opens when the game was started for
//! an agent (`--control`, `--control-socket`: horizon-ctl launch, the launcher's MCP `play`) or
//! when the player turned Agent Control on, and never under No AI (`horizon_core::no_ai`).
//! Turning either switch in the game opens or closes it at once.
use super::Game;
use horizon_client::{Result, control};
use std::path::PathBuf;

impl Game<'_> {
    /// The endpoint: the one the command line gave, else the default horizon-ctl looks for.
    fn agent_socket_path(&self) -> PathBuf {
        self.options
            .control
            .clone()
            .unwrap_or_else(control::default_path)
    }

    /// At startup: open the socket if the game was started for an agent or Agent Control is on.
    pub(super) fn start_agent_control(&mut self) -> Result<()> {
        self.agent_control_on = self.settings.extras.agent_control;
        if self.options.benchmark.is_some() && self.options.control.is_none() {
            return Ok(());
        }
        if self.settings.extras.no_ai {
            if self.options.control.is_some() {
                log::warn!(
                    "Horizon: control socket not opened: {}",
                    horizon_core::no_ai::REFUSAL
                );
            }
            return Ok(());
        }
        if let Some(path) = &self.options.control {
            // Asked for on the command line: failing to listen is an error.
            let server = control::Server::start(path)
                .map_err(|e| format!("control socket {}: {e}", path.display()))?;
            log::info!("Horizon: control socket {}", server.path().display());
            self.control = Some(server);
        } else if self.agent_control_on {
            self.open_agent_socket();
        }
        // The remote MCP server: a Streamable HTTP listener on every interface, so
        // AI clients reach the game. Open URL, no token, and
        // refused under No AI like every agent surface.
        if !self.settings.extras.no_ai && self.settings.extras.agent_control {
            let port = std::env::var("HORIZON_MCP_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(horizon_client::mcp_server::DEFAULT_PORT);
            let endpoint = horizon_client::logging::game_home().join("mcp-endpoint.json");
            match horizon_client::mcp_server::start(port, &endpoint) {
                Ok(server) => {
                    log::info!(
                        "Horizon: remote MCP (Streamable HTTP) listening on 0.0.0.0:{} (/mcp, endpoint in {})",
                        server.port(),
                        endpoint.display()
                    );
                    self.mcp = Some(server);
                }
                Err(e) => log::warn!("Horizon: remote MCP server: {e}"),
            }
        }
        Ok(())
    }

    /// The setting's socket: a failure (another game holds the default endpoint) is logged and
    /// shown on the AI Agent page, not fatal.
    fn open_agent_socket(&mut self) {
        let path = self.agent_socket_path();
        match control::Server::start(&path) {
            Ok(server) => {
                log::info!("Horizon: control socket {}", server.path().display());
                self.control = Some(server);
            }
            Err(e) => log::warn!("Horizon: control socket {}: {e}", path.display()),
        }
    }

    /// After a settings change: No AI closes the socket; Agent Control turned on or off opens or
    /// closes it.
    pub(super) fn sync_agent_control(&mut self) {
        let wanted = self.settings.extras.agent_control;
        let changed = wanted != self.agent_control_on;
        self.agent_control_on = wanted;
        let open = self
            .control
            .as_ref()
            .is_some_and(|c| !c.path().as_os_str().is_empty());
        if self.settings.extras.no_ai || (changed && !wanted) {
            if open {
                log::info!("Horizon: control socket closed");
                self.control = None;
            }
            // No AI, or Agent Control turned off: the remote MCP goes too.
            self.mcp = None;
        } else if changed && wanted && self.control.is_none() {
            self.open_agent_socket();
        }
        if wanted
            && !self.settings.extras.no_ai
            && self.mcp.is_none()
            && (changed || self.agent_control_on)
        {
            let port = std::env::var("HORIZON_MCP_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(horizon_client::mcp_server::DEFAULT_PORT);
            let endpoint = horizon_client::logging::game_home().join("mcp-endpoint.json");
            match horizon_client::mcp_server::start(port, &endpoint) {
                Ok(server) => {
                    log::info!(
                        "Horizon: remote MCP (Streamable HTTP) listening on 0.0.0.0:{} (/mcp, endpoint in {})",
                        server.port(),
                        endpoint.display()
                    );
                    self.mcp = Some(server);
                }
                Err(e) => log::warn!("Horizon: remote MCP server: {e}"),
            }
        }
    }

    /// An agent's command arrived.
    pub(super) fn agent_command_seen(&mut self) {
        self.agent_last = Some(std::time::Instant::now());
    }

    /// The AI Agent page's lines: the state, then where agents connect.
    pub(super) fn agent_status_lines(&self) -> Vec<String> {
        let server = self
            .control
            .as_ref()
            .filter(|c| !c.path().as_os_str().is_empty());
        let mut lines = vec![match server {
            Some(s) if s.clients() > 0 => {
                let ago = self
                    .agent_last
                    .map(|t| format!(", last command {} s ago", t.elapsed().as_secs()))
                    .unwrap_or_default();
                format!(
                    "Connected: {} agent connection{}{ago}",
                    s.clients(),
                    if s.clients() == 1 { "" } else { "s" }
                )
            }
            Some(_) => "Waiting for an agent.".into(),
            None => "Off: agents can't reach this game.".into(),
        }];
        if let Some(s) = server {
            if self.options.control.is_some() && !self.agent_control_on {
                lines.push("Opened for this session by the program that started the game.".into());
            }
            // The full path is too wide for the page: Copy Connection Details has it.
            let name = s.path().file_name().unwrap_or_default().to_string_lossy();
            lines.push(format!(
                "Socket: {name} (copy the details for its full path)"
            ));
        }
        lines.push("Local only: agents on this computer, never the network.".into());
        // The remote MCP server: its URL, for MCP clients (this device or LAN).
        match self.mcp.as_ref() {
            Some(m) => {
                lines.push(format!(
                    "Remote MCP: http://127.0.0.1:{}/mcp (LAN: http://<device IP>:{}/mcp)",
                    m.port(),
                    m.port()
                ));
            }
            None => {
                lines.push("Remote MCP: off (turn Agent Control on).".into());
            }
        }
        lines
    }

    /// What Copy Connection Details puts on the clipboard: the socket, a horizon-ctl line to try,
    /// and an MCP client's config for this game.
    pub(super) fn agent_details(&self) -> String {
        let socket = self.agent_socket_path();
        let ctl = std::env::current_exe()
            .unwrap_or_default()
            .with_file_name(if cfg!(windows) {
                "horizon-ctl.exe"
            } else {
                "horizon-ctl"
            });
        let config = serde_json::json!({ "mcpServers": { "horizon-game": {
            "command": ctl, "args": ["--socket", socket, "mcp"] } } });
        let mut text = format!(
            "Horizon agent control (this computer only)\nSocket: {}\nTry it: \"{}\" --socket \"{}\" state\nMCP config:\n{}\n",
            socket.display(),
            ctl.display(),
            socket.display(),
            serde_json::to_string_pretty(&config).unwrap_or_default()
        );
        // The remote MCP server: a Streamable HTTP client config, keyed by
        // the phone's own addresses since the listener is on every interface.
        if let Some(m) = self.mcp.as_ref() {
            let mcp_config = serde_json::json!({ "mcpServers": { "horizon-remote": {
                "url": format!("http://127.0.0.1:{}/mcp", m.port()),
                "note": "on another device use http://<this phone's IP>:<port>/mcp; tools: nbt_get, nbt_merge, run_command, build_command_chain",
            } } });
            text.push_str(&format!(
                "\nRemote MCP (Streamable HTTP): http://127.0.0.1:{}/mcp\n{}\n",
                m.port(),
                serde_json::to_string_pretty(&mcp_config).unwrap_or_default()
            ));
        }
        text
    }
}
