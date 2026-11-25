// Mount the existing passkey-backed management panels inside the app shell.
(() => {
  const { register, el, refresh } = window.pigeonApp;
  const panels = new Map();
  const { post } = window.pigeonPasskey;

  function element(tag, attrs = {}, ...children) {
    const { onclick, onkeydown, style, maxlength, ...safe } = attrs;
    // Uploaded avatars are limited to the same raster formats as the Directory.
    const avatar = tag === "img" && /^data:image\/(?:png|jpeg|webp);base64,/.test(safe.src);
    const node = el(tag, avatar ? { ...safe, src: null } : safe, children);
    if (avatar) node.src = safe.src;
    if (onclick) node.addEventListener("click", onclick);
    if (onkeydown) node.addEventListener("keydown", onkeydown);
    if (style) node.style.cssText = style;
    if (maxlength) node.maxLength = Number(maxlength);
    return node;
  }

  window.pigeonConsole = Object.freeze({
    el: element,
    post,
    section: (id, render) => panels.set(id, render),
    expiryPill: (row) => el("span", { class: "pill", text: row.expires || row.status }),
  });

  async function panel(id, { signal }) {
    const response = await fetch(`/console/api/section/${id}?fresh=1`, { signal });
    if (response.status === 401) { location.reload(); throw new Error("Sign in first."); }
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || `Request failed (${response.status}).`);
    const body = el("div", { id, class: "app-management" });
    panels.get(id)(body, data, {
      reload: () => { if (!signal.aborted) refresh(); },
    });
    queueMicrotask(() => {
      if (location.hash) requestAnimationFrame(() => {
        try { document.getElementById(decodeURIComponent(location.hash.slice(1)))?.scrollIntoView(); }
        catch { /* An invalid bookmark fragment can be ignored. */ }
      });
    });
    return body;
  }

  register("approvals", (ctx) => panel("approvals", ctx));
  register("instances", (ctx) => panel("instances", ctx));
  register("management", {
    subtabs: { credentials: "Credentials", people: "People", delegation: "Delegation" },
    render: (ctx) => panel(ctx.subtab, ctx),
  });
})();
