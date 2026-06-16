#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use {
    std::{
        env,
        io::prelude::*,
        net::Ipv4Addr,
        path::PathBuf,
        pin::Pin,
        sync::Arc,
    },
    async_mpd::MpdClient,
    clap_complete::engine::{
        ArgValueCompleter,
        PathCompleter,
    },
    directories::UserDirs,
    futures::{
        future,
        stream::{
            self,
            Stream,
            StreamExt as _,
            TryStreamExt as _,
        },
    },
    itertools::Itertools as _,
    lazy_regex::regex_captures,
    log_lock::*,
    path_slash::PathBufExt as _,
    percent_encoding::percent_decode_str,
    rand::{
        prelude::*,
        rng,
    },
    tokio::io::{
        self,
        AsyncBufReadExt as _,
        BufReader,
    },
    tokio_stream::wrappers::LinesStream,
    wheel::{
        fs::{
            self,
            File,
        },
        traits::IoResultExt as _,
    },
};
#[cfg(windows)] use directories::BaseDirs;
#[cfg(not(windows))] use {
    std::process::{
        Command,
        Stdio,
    },
    wheel::traits::SyncCommandOutputExt as _,
};

fn get_mpd_conf() -> Option<PathBuf> {
    #[cfg(windows)] {
        let path = BaseDirs::new()?.data_local_dir().join("mpd").join("mpd.conf");
        if path.exists() { return Some(path) }
        let path = UserDirs::new()?.home_dir().join(".config").join("mpd").join("mpd.conf");
        if path.exists() { return Some(path) }
    }
    #[cfg(not(windows))] {
        //TODO get config from `systemctl --user show mpd.service --property=ExecStart`
        if let Ok(exec_start) = Command::new("systemctl").arg("--user").arg("show").arg("mpd.service").arg("--property=ExecStart").stdout(Stdio::piped()).check("systemctl show")
        && let Ok(exec_start) = String::from_utf8(exec_start.stdout)
        && let Some((_, attrs)) = regex_captures!(r"^ExecStart=\{ (.*) \}$", exec_start.trim())
        && let Some(argv) = attrs.split(" ; ").find_map(|attr| attr.strip_prefix("argv[]="))
        && let Some(argv) = shlex::split(argv)
        && let Some(argv) = argv.get(1..)
        && let Ok(path) = argv.iter().filter(|arg| !arg.starts_with('-')).exactly_one()
        { return Some(path.into()) }
        if let Some(path) = xdg::BaseDirectories::new().find_config_file("mpd/mpd.conf") { return Some(path) }
    }
    let path = UserDirs::new()?.home_dir().join(".mpdconf");
    if path.exists() { return Some(path) }
    let path = UserDirs::new()?.home_dir().join(".mpd/mpd.conf");
    if path.exists() { return Some(path) }
    None
}

fn get_mpd_root() -> Result<PathBuf, Error> {
    use std::io::prelude::*;

    Ok(if let Some(mpd_root) = env::var_os("MPD_ROOT") {
        PathBuf::from(mpd_root)
    } else if let Some(conf) = get_mpd_conf()
        && let Some(music_dir) = std::io::BufReader::new(std::fs::File::open(&conf).at(&conf)?)
            .lines()
            .process_results(|mut lines| lines.find_map(|line| regex_captures!("^ *music_directory +\"(.+)\" *$", &line).map(|(_, music_dir)| music_dir.into()))).at(conf)?
    {
        music_dir
    } else {
        UserDirs::new().ok_or(Error::MissingHomeDir)?.audio_dir().ok_or(Error::MissingMusicDir)?.to_owned()
    })
}

fn get_tracks(mpd_root: Arc<Mutex<Option<PathBuf>>>, path: PathBuf) -> Pin<Box<dyn Stream<Item = Result<PathBuf, Error>> + Send>> {
    stream::once(async move {
        let absolute_path = if path.is_relative() {
            lock!(mpd_root = mpd_root; {
                if mpd_root.is_none() {
                    *mpd_root = Some(get_mpd_root()?);
                }
                mpd_root.as_ref().unwrap().join(&path)
            })
        } else {
            path.clone()
        };
        Ok::<_, Error>(if fs::metadata(&absolute_path).await?.is_dir() {
            fs::read_dir(absolute_path).and_then(move |entry| future::ok(get_tracks(mpd_root.clone(), entry.path()))).try_flatten().boxed()
        } else if absolute_path.extension().is_some_and(|ext| ext == "m3u8") {
            LinesStream::new(BufReader::new(File::open(&absolute_path).await?).lines())
                .map(move |res| res.at(&absolute_path))
                .map_err(Error::from)
                .map_ok(|line| line.trim().to_owned())
                .try_filter(|line| future::ready(!line.is_empty() && !line.starts_with('#')))
                .and_then(move |line| {
                    let mpd_root = mpd_root.clone();
                    async move { Ok(get_tracks(mpd_root, PathBuf::from(percent_decode_str(&line).decode_utf8()?.into_owned()))) }
                })
                .try_flatten()
                .boxed()
        } else if absolute_path.file_name().is_some_and(|filename| filename == "desktop.ini") {
            stream::empty().boxed()
        } else {
            lock!(mpd_root = mpd_root; {
                if mpd_root.is_none() {
                    *mpd_root = Some(get_mpd_root()?);
                }
                stream::once(future::ok(absolute_path.strip_prefix(mpd_root.as_ref().unwrap())?.to_owned())).boxed()
            })
        })
    }).try_flatten().boxed()
}

#[derive(clap::Parser)]
struct Args {
    #[clap(subcommand)]
    subcommand: Option<Subcommand>,
}

fn path_completer() -> ArgValueCompleter {
    let mut completer = PathCompleter::any();
    if let Ok(mpd_root) = get_mpd_root() {
        completer = completer.current_dir(mpd_root);
    }
    ArgValueCompleter::new(completer)
}

#[derive(clap::Subcommand)]
enum Subcommand {
    AddShuffled {
        #[clap(add = path_completer())]
        path: PathBuf,
    },
    List,
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)] Mpd(#[from] async_mpd::Error),
    #[error(transparent)] StripPrefix(#[from] std::path::StripPrefixError),
    #[error(transparent)] Utf8(#[from] std::str::Utf8Error),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[error("could not determine user folder")]
    MissingHomeDir,
    #[error("could not determine music folder")]
    MissingMusicDir,
    #[error("non-UTF-8 track path")]
    NonUtf8TrackPath,
}

#[wheel::main]
async fn main(Args { subcommand }: Args) -> Result<(), Error> {
    let mut mpc = MpdClient::new();
    mpc.connect((Ipv4Addr::LOCALHOST, 6600)).await?; // IPv4 has better compat than IPv6
    match subcommand {
        Some(Subcommand::AddShuffled { path }) => {
            let mut tracks = get_tracks(Arc::default(), path).try_collect::<Vec<_>>().await?;
            tracks.shuffle(&mut rng());
            for track in tracks {
                mpc.queue_add(&track.to_slash().ok_or(Error::NonUtf8TrackPath)?).await?;
            }
        }
        None | Some(Subcommand::List) => {
            let mut lock = std::io::stdout().lock();
            for track in mpc.queue().await? {
                match writeln!(lock, "{}", track.file) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => break,
                    Err(e) => return Err(e).at_unknown().map_err(Error::from),
                }
            }
        }
    }
    Ok(())
}
