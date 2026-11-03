const HOST = "org.focaldesk.browser";
const api = globalThis.browser ?? globalThis.chrome;

function askFocalDesk(prompt, page) {
  return api.runtime.sendNativeMessage(HOST, { prompt, page });
}

api.runtime.onInstalled.addListener(() => {
  api.contextMenus.create({ id: "ask-selection", title: "Ask FocalDesk about selection", contexts: ["selection"] });
  api.contextMenus.create({ id: "ask-page", title: "Ask FocalDesk about this page", contexts: ["page"] });
});

api.contextMenus.onClicked.addListener(async (info, tab) => {
  const text = info.menuItemId === "ask-selection" ? info.selectionText : "";
  const page = { title: tab.title || "", url: tab.url || "", text };
  const prompt = text ? `Answer this selected text from ${page.url}:\n${text}` : `Summarize this page: ${page.title}`;
  const reply = await askFocalDesk(prompt, page);
  await api.storage.local.set({ lastReply: reply.content || reply.error, lastPage: page.url });
  api.action.setBadgeText({ text: reply.error ? "!" : "OK", tabId: tab.id });
});

api.action.onClicked.addListener(async (tab) => {
  const reply = await askFocalDesk(`Summarize the page titled: ${tab.title || "untitled"}`, { title: tab.title || "", url: tab.url || "", text: "" });
  await api.storage.local.set({ lastReply: reply.content || reply.error, lastPage: tab.url || "" });
  api.action.setBadgeText({ text: reply.error ? "!" : "OK", tabId: tab.id });
});
