use crate::Error;

use futures::{Stream, StreamExt};
use std::{path::Path, sync::Arc};
use tokio::{
    fs::{self, File},
    io::copy,
};

pub async fn concatenator<P>(dest: &mut File, mut parts: P) -> Result<(), Error>
where
    P: Stream<Item = Result<Arc<Path>, Error>> + Unpin,
{
    while let Some(task_result) = parts.next().await {
        let part_path: Arc<Path> = task_result?;
        concatenate(dest, part_path).await?;
    }

    Ok(())
}

async fn concatenate(
    concatenated_file: &mut File,
    part_path: Arc<Path>,
) -> Result<(), Error> {
    let mut file = File::open(&*part_path)
        .await
        .map_err(|why| Error::OpenPart(part_path.clone(), why))?;

    copy(&mut file, concatenated_file).await.map_err(Error::Concatenate)?;

    if let Err(why) = fs::remove_file(&*part_path).await {
        error!("failed to remove part file ({:?}): {}", part_path, why);
    }

    Ok(())
}
