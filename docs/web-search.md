# Local AI web search

FocalDesk exposes web search as the read-only `web_search` agent tool. It does
not give the model arbitrary network access: the tool talks only to a local
SearXNG HTTP endpoint and returns no fetched-page content. Results are bounded
to eight entries containing a title, URL, short snippet, source engine, and
published date when available.

## Configure SearXNG

Run SearXNG locally and bind it to loopback only. The default endpoint is
`http://127.0.0.1:8080`. To use another local endpoint, create a systemd user
drop-in for `focaldesk-server.service`:

```ini
[Service]
Environment=FOCALDESK_SEARXNG_URL=http://127.0.0.1:8080
```

Then reload user units and restart FocalDesk Server:

```sh
systemctl --user daemon-reload
systemctl --user restart focaldesk-server.service
```

`FOCALDESK_SEARXNG_URL` accepts only unauthenticated `http` loopback URLs
(`localhost`, `127.0.0.1`, or `[::1]`) with no query or fragment. A remote
endpoint, embedded credentials, redirects, result-page fetching, and page
automation are deliberately unsupported.

## Use it

The built-in Desktop Assistant includes `web_search`. Select **Desktop
Assistant** in the AI Console and ask it to search the web. Through Ambient
Voice, route the request to that agent, for example: “Hello FocalDesk, ask
agent desktop to search the web for local SearXNG documentation.” The response
should cite the returned result URLs. If SearXNG is not running, the tool fails
closed and the assistant reports that local search is unavailable.
