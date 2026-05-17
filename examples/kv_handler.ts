/**
 * This is a simple in Memory KV example.
 * You may use this pattern as a proxy handler to another KV store (e.g. Cloudflare Workers + KV, a Redis server etc.).
 * Or simply port the implementation.
 * 
 * XHFS's 'kvhttp' device works as the following:
 * PUT `/xhfs/{key_string}`
 *  -> XHFS will send raw binary blobs in the response body
 * 
 * GET `/xhfs/{key_string}`
 *  -> Similarly, the handler side must return raw binary blobs
 *  -> If the key is not present, XHFS expects 0 bytes in the response body
 * 
 * Run
 * deno run -A examples/kv_handler.ts
 * # init only
 * xhfs format --config examples/kv_handler.ts
 * # check if everything is working
 * xhfs infos --config examples/kv_handler.ts
 */

const kvStore = new Map<string, Uint8Array>();

const stats = { read: 0, write: 0 };
Deno.serve(async (req) => {
  const url = new URL(req.url);

  const [, handler, ...rest] = url.pathname.split("/");
  const key = rest.join("/");

  if (stats.write % 1000 == 0 || stats.read % 1000 == 0) {
    console.log(new Date().toLocaleTimeString(), stats);
  }

  if (handler === "") {
    return new Response(JSON.stringify(stats), {
      headers: {
        "content-type": "application/json",
      },
    });
  }

  if (handler !== "xhfs") {
    return new Response("Not found", { status: 404 });
  }

  if (req.method === "PUT") {
    const body = new Uint8Array(await req.arrayBuffer());
    stats.write += 1;
    kvStore.set(key, body); // !
    return new Response("OK");
  }

  if (req.method == "GET") {
    const value = kvStore.get(key) ?? new Uint8Array(0);
    stats.read += 1;
    return new Response(value, {
      headers: {
        "content-type": "application/octet-stream",
      },
    });
  }

  if (req.method === "DELETE") {
    kvStore.delete(key);
    return new Response("Deleted");
  }

  return new Response("Method not allowed", { status: 405 });
});
