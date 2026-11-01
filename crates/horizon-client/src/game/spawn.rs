//! `horizon-ctl spawn NAME`: another player in a headless game's process (#4952).
//!
//! A headless player is mostly the game's shared tables: block states, models, recipes and
//! the rest of `Resources` and `PackResources` (about 370 MB), against a few tens of MB for
//! its world. Each player in a process of its own pays for those tables again; a spawned
//! player is a second client `Game` on its own thread that shares them through their `Arc`s,
//! as the integrated server's `Game` does. It joins a server, plays through its own control
//! socket, and leaves when that socket is told to `quit`. Players can be added at any time.
//!
//! A spawned player is blind: its GUI resources are the fonts only (`UiResources::server`),
//! so `vision` refuses. Its account is its own (`auth::set_for_thread`): the signed-in account
//! of a session file from the launcher, never the account the process started with. Like the
//! rest of Horizon it follows Mojang's rules: no player without an account that owns the game.

use super::{Audio, Game, UiResources, start_game};
use horizon_client::Result;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Instant,
};

/// A spawned player plays as a signed-in account that owns the game, as every Horizon player.
const NO_SESSION: &str =
    "spawn needs --session-file: the signed-in account it plays as, from the launcher";

/// FORK ONLY (Solenopsisbot/horizon main, never upstream): without a session file a spawned
/// player plays offline as NAME, for bots on our own offline servers. Upstream refuses it
/// (`NO_SESSION`): Horizon follows Mojang's rules there.
const OFFLINE_BOTS: bool = true;

/// The tables a game builds for itself (entity and frame models, the creative inventory),
/// loaded once for every player the process spawns: each gets a clone, whose heavy parts are
/// shared, instead of parsing the entity models again (20-30 MB of work each).
static CPU_TEMPLATE: Mutex<Option<super::GameCpuResources>> = Mutex::new(None);

fn cpu_template(root: &Path) -> Result<super::GameCpuResources> {
    let mut template = CPU_TEMPLATE.lock().unwrap_or_else(|e| e.into_inner());
    if template.is_none() {
        *template = Some(super::GameCpuResources::load(root)?);
    }
    Ok(template.as_ref().map(Clone::clone).expect("loaded above"))
}

/// The names of the players spawned in this process and still playing.
static PLAYING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// A spawned player's control socket: the first player's, with `-NAME` before its extension.
fn socket_beside(first: &Path, name: &str) -> PathBuf {
    let stem = first
        .file_stem()
        .map_or_else(|| "horizon".into(), |s| s.to_string_lossy().into_owned());
    let file = match first.extension() {
        Some(ext) => format!("{stem}-{name}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{name}"),
    };
    first.with_file_name(file)
}

impl Game<'_> {
    /// Starts player `name` in this process and returns its control socket. It joins
    /// `server` (default: the server this game was started on) as the signed-in account in
    /// `session_file`, asking for `render_distance` chunks around it (default: this game's).
    /// It is still connecting when this returns: `wait-ready` on its socket says when it has
    /// spawned.
    pub(super) fn add_player(
        &self,
        name: &str,
        server: Option<&str>,
        session_file: Option<&Path>,
        render_distance: Option<i32>,
    ) -> Result<PathBuf> {
        if !self.options.headless {
            return Err("spawn adds players to a game started with --headless".into());
        }
        if !horizon_client::offline_name::is_valid(name) {
            return Err(format!("{name} is not a player name (1 to 16 characters)").into());
        }
        let first = self
            .options
            .control
            .as_deref()
            .ok_or("spawn needs a game started with a control socket (--control-socket)")?;
        let server = server
            .map(str::to_owned)
            .or_else(|| self.options.server.clone())
            .ok_or("spawn joins a server: give one (--server HOST[:PORT])")?;
        // Read now, so a bad file is this command's error, not a silent offline player.
        let session = session_file
            .map(horizon_client::auth::AuthSession::load)
            .transpose()?;
        if session.is_none() && !OFFLINE_BOTS {
            return Err(NO_SESSION.into());
        }
        let name = session
            .as_ref()
            .map_or(name, |s| s.name.as_str())
            .to_owned();
        {
            let mut playing = PLAYING.lock().unwrap_or_else(|e| e.into_inner());
            if !playing
                .get_or_insert_with(HashSet::new)
                .insert(name.clone())
            {
                return Err(format!("{name} is already playing in this game").into());
            }
        }
        let socket = socket_beside(first, &name);
        let mut options = self.options.clone();
        options.spawned = true;
        options.player_name = Some(name.clone());
        options.server = Some(server);
        options.control = Some(socket.clone());
        options.world = None;
        options.quick_play = None;
        options.session_file = None;
        // Most of a spawned player's memory is its chunks: a bot that needs only what is
        // around it asks the server for fewer (vanilla's minimum is 2).
        if let Some(distance) = render_distance {
            options.distance_override = Some(distance.clamp(2, 32));
        }
        let (res, pack, settings) = (self.res.clone(), self.pack.clone(), self.settings.clone());
        let cpu = cpu_template(&options.resources)?;
        let vanilla_root = self.ui.vanilla_root.clone();
        let thread_name = name.clone();
        let started = std::thread::Builder::new()
            .name(format!("Player {name}"))
            .spawn(move || {
                horizon_client::auth::set_for_thread(session);
                let run = || -> Result<()> {
                    let gui = UiResources::server(&options.resources)?;
                    let audio = Audio::silent(&options.resources);
                    let Some(mut game) = start_game(
                        &res,
                        pack,
                        gui,
                        options,
                        None,
                        audio,
                        vanilla_root,
                        Instant::now(),
                        None,
                        Some(settings),
                        Some(cpu),
                    )?
                    else {
                        return Ok(());
                    };
                    game.run_headless();
                    game.error.take().map_or(Ok(()), Err)
                };
                if let Err(e) = run() {
                    log::error!("Horizon: player {thread_name}: {e}");
                }
                if let Some(playing) = PLAYING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    playing.remove(&thread_name);
                }
            });
        if let Err(e) = started {
            if let Some(playing) = PLAYING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                playing.remove(&name);
            }
            return Err(e.into());
        }
        log::info!(
            "Horizon: spawned player {name}, control socket {}",
            socket.display()
        );
        Ok(socket)
    }

    /// Worker threads for a server's world: the machine's (`Resources::threads`), but one for
    /// a spawned player. It is blind, so it meshes nothing, and a server sends its chunks lit:
    /// a pool of every core but one would be that many idle threads for each player.
    pub(super) fn remote_workers(&self) -> usize {
        if self.options.spawned {
            1
        } else {
            self.res.threads()
        }
    }

    /// The players spawned in this process and still playing, by name.
    pub(super) fn spawned_players() -> Vec<String> {
        let mut names: Vec<String> = PLAYING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .flatten()
            .cloned()
            .collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spawned_players_socket_sits_beside_the_first() {
        assert_eq!(
            socket_beside(Path::new("/run/bots/ctl.sock"), "Bot_2"),
            Path::new("/run/bots/ctl-Bot_2.sock")
        );
        assert_eq!(
            socket_beside(Path::new("/tmp/horizon-control"), "Alex"),
            Path::new("/tmp/horizon-control-Alex")
        );
    }
}
