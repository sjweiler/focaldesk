use crate::{BrowserRequest, validate_url};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chromiumoxide::{
    Page,
    browser::{Browser, BrowserConfig},
    cdp::browser_protocol::{
        browser::{SetDownloadBehaviorBehavior, SetDownloadBehaviorParams},
        page::{CaptureScreenshotFormat, GetNavigationHistoryParams, NavigateToHistoryEntryParams},
    },
    page::ScreenshotParams,
};
use futures::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;

struct Tab {
    page: Page,
    used: Instant,
}
pub struct BrowserEngine {
    browser: Option<Browser>,
    handler: Option<JoinHandle<()>>,
    pages: HashMap<String, Tab>,
    next: u64,
}
impl Default for BrowserEngine {
    fn default() -> Self {
        Self {
            browser: None,
            handler: None,
            pages: HashMap::new(),
            next: 0,
        }
    }
}
impl BrowserEngine {
    async fn launch(&mut self) -> Result<()> {
        if self.handler.as_ref().is_some_and(|h| h.is_finished()) {
            self.shutdown().await;
        }
        if self.browser.is_some() {
            return Ok(());
        }
        let profile = crate::socket_path()?.with_extension("profile");
        tokio::fs::create_dir_all(&profile).await?;
        let mut config = BrowserConfig::builder()
            .user_data_dir(profile)
            .request_timeout(Duration::from_secs(25))
            .viewport(chromiumoxide::handler::viewport::Viewport {
                width: 1280,
                height: 800,
                ..Default::default()
            })
            .arg("--disable-background-networking")
            .arg("--disable-extensions")
            .arg("--no-first-run");
        if let Some(exe) = std::env::var_os("FOCALDESK_CHROMIUM_EXECUTABLE") {
            config = config.chrome_executable(exe);
        }
        let (browser, mut handler) =
            Browser::launch(config.build().map_err(|e| anyhow::anyhow!(e))?)
                .await
                .context(
                    "Launch Chromium (set FOCALDESK_CHROMIUM_EXECUTABLE if detection fails)",
                )?;
        self.handler = Some(tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if event.is_err() {
                    break;
                }
            }
        }));
        self.browser = Some(browser);
        self.browser
            .as_ref()
            .unwrap()
            .execute(SetDownloadBehaviorParams {
                behavior: SetDownloadBehaviorBehavior::Deny,
                browser_context_id: None,
                download_path: None,
                events_enabled: None,
            })
            .await?;
        Ok(())
    }
    pub async fn shutdown(&mut self) {
        self.pages.clear();
        if let Some(mut b) = self.browser.take() {
            let _ = tokio::time::timeout(Duration::from_secs(3), b.close()).await;
        }
        if let Some(h) = self.handler.take() {
            h.abort();
        }
    }
    pub async fn handle(&mut self, request: BrowserRequest) -> Result<Value> {
        if matches!(request, BrowserRequest::Status) {
            return Ok(
                json!({"running":self.browser.is_some()&&!self.handler.as_ref().is_some_and(|h|h.is_finished()),"pages":self.pages.len(),"page_ids":self.pages.keys().collect::<Vec<_>>(),"engine":"chromiumoxide","profile":"dedicated"}),
            );
        }
        self.launch().await?;
        let expired = self
            .pages
            .iter()
            .filter(|(_, t)| t.used.elapsed() > Duration::from_secs(900))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in expired {
            if let Some(tab) = self.pages.remove(&id) {
                let _ = tab.page.close().await;
            }
        }
        match request {
            BrowserRequest::Open { url } => {
                validate_url(&url)?;
                if self.pages.len() >= 8 {
                    bail!("Eight pages are open; close a page before opening another");
                }
                let page = self
                    .browser
                    .as_ref()
                    .unwrap()
                    .new_page("about:blank")
                    .await?;
                self.next += 1;
                let id = format!("{}-{}", std::process::id(), self.next);
                self.pages.insert(
                    id.clone(),
                    Tab {
                        page: page.clone(),
                        used: Instant::now(),
                    },
                );
                if let Err(error) = page.goto(url).await {
                    self.pages.remove(&id);
                    let _ = page.close().await;
                    return Err(error.into());
                }
                match read_page(&page, &id).await {
                    Ok(value) => Ok(value),
                    Err(error) => {
                        self.pages.remove(&id);
                        let _ = page.close().await;
                        Err(error)
                    }
                }
            }
            BrowserRequest::Close { page_id } => {
                let tab = self
                    .pages
                    .remove(&page_id)
                    .context("Page not found or expired")?;
                tab.page.close().await?;
                Ok(json!({"closed":page_id}))
            }
            request => {
                let id = match &request {
                    BrowserRequest::Read { page_id }
                    | BrowserRequest::Navigate { page_id, .. }
                    | BrowserRequest::Back { page_id }
                    | BrowserRequest::Forward { page_id }
                    | BrowserRequest::Reload { page_id }
                    | BrowserRequest::Screenshot { page_id }
                    | BrowserRequest::Click { page_id, .. }
                    | BrowserRequest::Type { page_id, .. } => page_id.clone(),
                    _ => unreachable!(),
                };
                let tab = self
                    .pages
                    .get_mut(&id)
                    .context("Page not found or expired; open it again")?;
                tab.used = Instant::now();
                let page = tab.page.clone();
                validate_url(&page.url().await?.context("Page has no URL")?)?;
                match request {
                    BrowserRequest::Navigate { url, .. } => {
                        validate_url(&url)?;
                        page.goto(url).await?;
                        read_page(&page, &id).await
                    }
                    BrowserRequest::Reload { .. } => {
                        page.reload().await?;
                        read_page(&page, &id).await
                    }
                    action @ (BrowserRequest::Back { .. } | BrowserRequest::Forward { .. }) => {
                        let history = page
                            .execute(GetNavigationHistoryParams::default())
                            .await?
                            .result;
                        let target = history.current_index
                            + if matches!(action, BrowserRequest::Back { .. }) {
                                -1
                            } else {
                                1
                            };
                        let entry = history
                            .entries
                            .get(usize::try_from(target).unwrap_or(usize::MAX))
                            .context("No navigation history in that direction")?;
                        validate_url(&entry.url)?;
                        page.execute(NavigateToHistoryEntryParams::new(entry.id))
                            .await?;
                        // Back/forward may restore from BFCache without a network response.
                        // Waiting for an HTTP navigation here would never resolve.
                        let expected_url = entry.url.clone();
                        tokio::time::timeout(Duration::from_secs(20), async {
                            loop {
                                let current = page
                                    .execute(GetNavigationHistoryParams::default())
                                    .await?
                                    .result;
                                if current.current_index == target {
                                    if let Ok(result) = page
                                        .evaluate("({url:location.href,ready:document.readyState})")
                                        .await
                                    {
                                        if let Ok(value) = result.into_value::<Value>() {
                                            if value["url"].as_str() == Some(expected_url.as_str())
                                                && value["ready"] == "complete"
                                            {
                                                break;
                                            }
                                        }
                                    }
                                }
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                            Ok::<(), anyhow::Error>(())
                        })
                        .await
                        .context("History navigation did not finish")??;
                        read_page(&page, &id).await
                    }
                    BrowserRequest::Screenshot { .. } => {
                        let png = page
                            .screenshot(
                                ScreenshotParams::builder()
                                    .format(CaptureScreenshotFormat::Png)
                                    .full_page(false)
                                    .build(),
                            )
                            .await?;
                        if png.len() > 5 * 1024 * 1024 {
                            bail!("Screenshot exceeds 5 MB");
                        }
                        let screenshot_path = crate::socket_path()?
                            .parent()
                            .unwrap()
                            .join(format!("browser-{id}.png"));
                        tokio::fs::write(&screenshot_path, &png).await?;
                        Ok(
                            json!({"page_id":id,"url":page.url().await?,"screenshot_path":screenshot_path,"data_url":format!("data:image/png;base64,{}",STANDARD.encode(png))}),
                        )
                    }
                    BrowserRequest::Click { selector, .. } => {
                        validate_selector(&selector)?;
                        page.find_element(selector).await?.click().await?;
                        read_page(&page, &id).await
                    }
                    BrowserRequest::Type { selector, text, .. } => {
                        validate_selector(&selector)?;
                        if text.len() > 16 * 1024 {
                            bail!("Input exceeds 16 KB");
                        }
                        page.find_element(selector)
                            .await?
                            .focus()
                            .await?
                            .type_str(text)
                            .await?;
                        read_page(&page, &id).await
                    }
                    BrowserRequest::Read { .. } => read_page(&page, &id).await,
                    _ => unreachable!(),
                }
            }
        }
    }
}
fn validate_selector(selector: &str) -> Result<()> {
    if selector.trim().is_empty() || selector.len() > 1024 {
        bail!("CSS selector must contain 1-1024 bytes");
    }
    Ok(())
}
async fn read_page(page: &Page, id: &str) -> Result<Value> {
    let mut result:Value=page.evaluate(r#"(() => {
   const visible=e=>!!(e.getClientRects().length)&&getComputedStyle(e).visibility!=='hidden';
   const elements=Array.from(document.querySelectorAll('a[href],button,input,textarea,select,[role="button"]')).filter(visible).slice(0,100).map((e,i)=>{
     e.setAttribute('data-focaldesk-element',String(i));
     return {selector:'[data-focaldesk-element="'+i+'"]',tag:e.tagName.toLowerCase(),text:(e.innerText||e.getAttribute('aria-label')||e.getAttribute('placeholder')||'').slice(0,200),type:e.getAttribute('type'),href:e.tagName==='A'?e.href:undefined};
   });
   const text=(document.body?.innerText||'');
   return {url:location.href,title:document.title,text:text.slice(0,32000),truncated:text.length>32000,elements,untrusted_content:true};
 })()"#).await?.into_value()?;
    validate_url(result["url"].as_str().context("Page returned no URL")?)?;
    result["page_id"] = json!(id);
    Ok(result)
}
