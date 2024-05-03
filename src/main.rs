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
    percent_encoding::percent_decode_str,
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

fn get_tracks(path: PathBuf) -> Pin<Box<dyn Stream<Item = Result<PathBuf, Error>> + Send>> {
    stream::once(async move {
        let absolute_path = if path.is_relative() {
            match env::var_os("MPD_ROOT") {
                Some(mpd_root) => PathBuf::from(mpd_root).join(&path),
                None => UserDirs::new().ok_or(Error::MissingHomeDir)?.audio_dir().ok_or(Error::MissingMusicDir)?.join(&path),
            }
        } else {
            path.clone()
        };
        Ok::<_, Error>(if fs::metadata(&absolute_path).await?.is_dir() {
            fs::read_dir(absolute_path).and_then(|entry| future::ok(get_tracks(entry.path()))).try_flatten().boxed()
        } else if absolute_path.extension().is_some_and(|ext| ext == "m3u8") {
            LinesStream::new(BufReader::new(File::open(absolute_path).await?).lines())
                .map_err(Error::from)
                .map_ok(|line| line.trim().to_owned())
                .try_filter(|line| future::ready(!line.is_empty() && !line.starts_with('#')))
                .and_then(|line| async move { Ok(get_tracks(PathBuf::from(percent_decode_str(&line).decode_utf8()?.into_owned()))) })
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
    List,
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)] Io(#[from] io::Error),
    #[error(transparent)] Mpd(#[from] async_mpd::Error),
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
async fn main(args: Args) -> Result<(), Error> {
    let mut mpc = MpdClient::new();
    mpc.connect((Ipv6Addr::LOCALHOST, 6600)).await?;
    match args {
        Args::AddShuffled { path } => {
            let mut tracks = get_tracks(path).try_collect::<Vec<_>>().await?;
            tracks.shuffle(&mut thread_rng());
            for track in tracks {
                mpc.queue_add(track.to_str().ok_or(Error::NonUtf8TrackPath)?).await?;
            }
        }
        Args::List => for track in mpc.queue().await? {
            println!("{}", track.file);
        },
    }
    Ok(())
}
