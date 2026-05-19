/**
 * This is a 1-1 translation of kv_handler.ts
 *
 * Assumes you have `KV` namespace enabled.
 */
export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    const [, handler, ...rest] = url.pathname.split("/");
    const key = rest.join("/");

    if (handler !== "xhfs") {
      return new Response("Not found", { status: 404 });
    }

    if (request.method === "PUT") {
      const arrayBuffer = await request.arrayBuffer();
      await env.KV.put(key, arrayBuffer);
      return new Response("OK");
    }

    if (request.method === "GET") {
      const value = await env.KV.get(key, { type: "arrayBuffer" });
      const responseData = value ?? new ArrayBuffer(0);

      return new Response(responseData, {
        headers: {
          "content-type": "application/octet-stream",
        },
      });
    }

    if (request.method === "DELETE") {
      await env.KV.delete(key);
      return new Response("Deleted");
    }

    return new Response("Method not allowed", { status: 405 });
  },
};
