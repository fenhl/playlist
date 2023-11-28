use {
    std::{
        env,
        net::Ipv6Addr,
        path::PathBuf,
        pin::Pin,
    },
    async_mpd::MpdClient,
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
    rand::prelude::*,
    tokio::io::{
        self,
        AsyncBufReadExt as _,
        BufReader,
    },
    tokio_stream::wrappers::LinesStream,
    wheel::fs::{
        self,
        File,
    },
};

fn get_tracks(mut path: PathBuf) -> Pin<Box<dyn Stream<Item = Result<PathBuf, Error>> + Send>> {
    stream::once(async move {
        if path.is_relative() {
            path = match env::var_os("MPD_ROOT") {
                Some(mpd_root) => PathBuf::from(mpd_root).join(path),
                None => UserDirs::new().ok_or(Error::MissingHomeDir)?.audio_dir().ok_or(Error::MissingMusicDir)?.join(path),
            };
        }
        Ok::<_, Error>(if fs::metadata(&path).await?.is_dir() {
            fs::read_dir(path).and_then(|entry| future::ok(get_tracks(entry.path()))).try_flatten().boxed()
        } else if path.extension().is_some_and(|ext| ext == "m3u8") {
            LinesStream::new(BufReader::new(File::open(path).await?).lines())
                .map_err(Error::from)
                .map_ok(|line| line.trim().to_owned())
                .try_filter(|line| future::ready(!line.is_empty() && !line.starts_with('#')))
                .and_then(|line| future::ok(get_tracks(PathBuf::from(line))))
                .try_flatten()
                .boxed()
        } else {
            stream::once(future::ok(path)).boxed()
        })
    }).try_flatten().boxed()
}

#[derive(clap::Parser)]
enum Args {
    AddShuffled {
        path: PathBuf,
    },
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)] Io(#[from] io::Error),
    #[error(transparent)] Mpd(#[from] async_mpd::Error),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[error("could not determine user folder")]
    MissingHomeDir,
    #[error("could not determine music folder")]
    MissingMusicDir,
    #[error("non-UTF-8 track path")]
    Utf8,
}

#[wheel::main]
async fn main(args: Args) -> Result<(), Error> {
    let mut mpc = MpdClient::new();
    mpc.connect((Ipv6Addr::LOCALHOST, 6600)).await?;
    match args {
        Args::AddShuffled { path } => {
            let mut tracks = get_tracks(path).try_collect::<Vec<_>>().await?;
            tracks.shuffle(&mut thread_rng());
            for track in tracks {
                mpc.queue_add(track.to_str().ok_or(Error::Utf8)?).await?;
            }
        }
    }
    Ok(())
}
