// hidane.dev — tiny Worker in front of the static assets.
// 1. www.hidane.dev → hidane.dev (301)
// 2. /install.sh is served as a shell script (it installs the latest GitHub Release)
// Everything else is served from ./public by the assets binding.
export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const host = (request.headers.get("host") || url.host).split(":")[0].toLowerCase();
    if (host === "www.hidane.dev") {
      return Response.redirect("https://hidane.dev" + url.pathname + url.search, 301);
    }
    const res = await env.ASSETS.fetch(request);
    if (url.pathname === "/install.sh") {
      const headers = new Headers(res.headers);
      headers.set("content-type", "text/x-shellscript; charset=utf-8");
      headers.set("cache-control", "no-cache");
      return new Response(res.body, { status: res.status, headers });
    }
    return res;
  },
};
