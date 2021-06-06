use crate::{Error, FetchEvent, Fetcher, Source};

use futures::{Future, Stream, StreamExt};
use hyper::client::connect::Connect;
use std::{path::Path, sync::Arc};
use tokio::fs;

#[derive(new, Setters)]
pub struct FetcherSystem<C> {
    #[setters(skip)]
    client: Arc<Fetcher<C>>,
}

impl<C: Connect + Clone + Send + Sync + 'static> FetcherSystem<C> {
    pub fn build<I, T>(
        self,
        inputs: I,
    ) -> impl Stream<Item = impl Future<Output = (Arc<Path>, Result<T, Error>)>>
    where
        I: Stream<Item = (Source, T)> + Unpin + Send + 'static,
    {
        inputs.map(move |(source, extra)| {
            let fetcher = self.client.clone();

            async move {
                let Source { dest, urls, part, .. } = source;

                fetcher.send((dest.clone(), FetchEvent::Fetching));

                let result = match part {
                    Some(part) => {
                        match fetcher.clone().request(urls, part.clone()).await {
                            Ok(()) => {
                                fs::rename(&*part, &*dest).await.map_err(Error::Rename)
                            }
                            Err(why) => Err(why),
                        }
                    }
                    None => fetcher.clone().request(urls, dest.clone()).await,
                };

                fetcher.send((dest.clone(), FetchEvent::Fetched));

                (dest, result.map(|_| extra))
            }
        })
    }
}
