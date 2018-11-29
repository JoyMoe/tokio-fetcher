//! Provides a high level abstraction over a reqwest async client for optionally fetching
//! files from a remote location when required, checking that the downloaded file is valid, and
//! processing that downloaded file if processing is required (ie: decompression).
//!
//! # Features
//!
//! - Files will only be downloaded if the timestamp of the file is older than the timestamp on the server.
//! - Or if the destination checksum does not match, if a checksum is provided.
//! - Partial downloads will be stored at a temporary location, then renamed after completion.
//! - The fetched files will have their file times modified to match that of the server they fetched from.
//! - The checksum of the fetched file can be compared, as well as the checksum of the destination.
//! - The caller may optionally process the fetched file into the destination.
//! - Implemented as a state machine for creating requests. These can be converted to futures at any time:
//!   - `AsyncFetcher` -> `ResponseState`
//!   - `ResponseState` -> `FetchedState`
//!   - `FetchedState` -> `CompletedState`
//!
//! # Notes
//! - The generated futures are compatible with multi-threaded runtimes.
//! - See the examples directory in the source repository for an example of it in practice.
#[macro_use]
extern crate log;
#[macro_use]
extern crate failure_derive;

extern crate chrono;
extern crate digest;
extern crate failure;
extern crate filetime;
extern crate futures;
extern crate hex_view;
extern crate reqwest;
extern crate tokio;

mod errors;
mod hashing;
mod states;

use chrono::{DateTime, Utc};
use digest::Digest;
pub use errors::*;
use failure::ResultExt;
use filetime::FileTime;
use futures::{future::ok as OkFuture, Future, Stream};
use hashing::*;
use hex_view::HexView;
use reqwest::{
    async::{Client, RequestBuilder, Response},
    header::{IF_MODIFIED_SINCE, LAST_MODIFIED},
    StatusCode,
};
pub use states::*;
use std::{
    fs::{remove_file as remove_file_sync, File as SyncFile},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::fs::{remove_file, rename, File};

/// A future builder for creating futures to fetch files from an asynchronous reqwest client.
pub struct AsyncFetcher<'a> {
    client: &'a Client,
    from_url: String,
}

impl<'a> AsyncFetcher<'a> {
    /// Initialze a new featuer to fetch from the given URL.
    ///
    /// Stores the complete file to `to_path` when done.
    pub fn new(client: &'a Client, from_url: String) -> Self {
        Self { client, from_url }
    }

    /// Submit the GET request for the file, if the modified time is too old.
    ///
    /// Returns a `ResponseState`, which can either be manually handled by the caller, or used
    /// to commit the download with this API.
    pub fn request_to_path(
        self,
        to_path: PathBuf,
    ) -> ResponseState<
        impl Future<Item = Option<(Response, Option<DateTime<Utc>>)>, Error = reqwest::Error> + Send,
    > {
        let (req, current) = self.set_if_modified_since(&to_path, self.client.get(&self.from_url));
        let url = self.from_url;

        ResponseState {
            future: req
                .send()
                .and_then(|resp| resp.error_for_status())
                .map(move |resp| check_response(resp, &url, current)),
            path: to_path,
        }
    }

    /// Submit the GET request for the file, if checksums are a mismatch.
    ///
    /// Returns a `ResponseState`, which can either be manually handled by the caller, or used
    /// to commit the download with this API.
    pub fn request_with_checksum_to_path<D: Digest>(
        self,
        to_path: PathBuf,
        checksum: &str,
    ) -> ResponseState<
        impl Future<Item = Option<(Response, Option<DateTime<Utc>>)>, Error = reqwest::Error> + Send,
    > {
        let future: Box<
            dyn Future<Item = Option<(Response, Option<DateTime<Utc>>)>, Error = reqwest::Error>
                + Send,
        > = if hash_from_path::<D>(&to_path, &checksum).is_ok() {
            debug!("checksum of destination matches the requested checksum");
            Box::new(OkFuture(None))
        } else {
            let req = self.client.get(&self.from_url);
            let url = self.from_url;
            let future = req
                .send()
                .and_then(|resp| resp.error_for_status())
                .map(move |resp| check_response(resp, &url, None));
            Box::new(future)
        };

        ResponseState {
            future,
            path: to_path,
        }
    }

    fn set_if_modified_since(
        &self,
        to_path: &Path,
        mut req: RequestBuilder,
    ) -> (RequestBuilder, Option<DateTime<Utc>>) {
        let date = if to_path.exists() {
            let when = self.date_time(&to_path).unwrap();
            let rfc = when.to_rfc2822();
            req = req.header(IF_MODIFIED_SINCE, rfc);
            Some(when)
        } else {
            None
        };

        (req, date)
    }

    fn date_time(&self, to_path: &Path) -> io::Result<DateTime<Utc>> {
        Ok(DateTime::from(to_path.metadata()?.modified()?))
    }
}

fn check_response(
    resp: Response,
    url: &str,
    current: Option<DateTime<Utc>>,
) -> Option<(Response, Option<DateTime<Utc>>)> {
    if resp.status() == StatusCode::NOT_MODIFIED {
        debug!("{} was already fetched", url);
        None
    } else {
        let date = resp
            .headers()
            .get(LAST_MODIFIED)
            .and_then(|h| h.to_str().ok())
            .and_then(|header| DateTime::parse_from_rfc2822(header).ok())
            .map(|tz| tz.with_timezone(&Utc));

        let fetch = date
            .as_ref()
            .and_then(|server| current.map(|current| (server, current)))
            .map_or(true, |(&server, current)| current < server);

        if fetch {
            debug!("GET {}", url);
            Some((resp, date))
        } else {
            debug!("{} was already fetched", url);
            None
        }
    }
}
