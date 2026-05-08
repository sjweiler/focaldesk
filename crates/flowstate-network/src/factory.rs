use std::sync::Arc;
use anyhow::Result;

use crate::backend::NetworkBackend;
use crate::backend::networkmanager::NetworkManagerBackend;
use crate::backend::rtnetlink::RtnetlinkBackend;

pub async fn auto_backend() -> Result<Arc<dyn NetworkBackend>> {
    if let Ok(nm) = NetworkManagerBackend::new().await {
        return Ok(Arc::new(nm));
    }

    Ok(Arc::new(RtnetlinkBackend::new()?))
}

