// A real MCP server over stdio for the DAILY-USE-BACKEND-01 r2 journeys.
//
// DELIBERATELY A SEPARATE IMPLEMENTATION from the product's own fixture, so a
// journey against it is not the product agreeing with itself. It is still a
// test server, and this file says so rather than letting a receipt imply
// otherwise.
//
// TWO PROPERTIES THIS VERSION ADDS, and both exist because the previous one
// could not witness what I claimed for it.
//
// 1. THE RESULT IS NOT DERIVABLE FROM THE ARGUMENTS. The old tool answered
//    `journey_echo saw: ${text}`. A journey asserting that text proves nothing:
//    the model supplied the text, so it could render that line having never
//    called any tool. Every answer now carries SERVER_TOKEN, a constant that
//    appears in this file and nowhere in any prompt, plus a transform of the
//    input the model is not asked to perform. Seeing them on screen means the
//    call actually reached this process and its output actually came back.
//
// 2. `notifications/initialized` IS ENFORCED. The old server accepted
//    `tools/list` whether or not the notification ever arrived, so a journey
//    reaching "connected" said nothing about it -- which is why I narrowed that
//    claim in JOURNEY-01-RECEIPT.md. Here the notification is REQUIRED before
//    tools are listed or called, so a product that skips it cannot reach a
//    green journey and the handshake step is witnessed rather than assumed.

const SERVER_TOKEN = "ZULUQUEBEC7731";

// WHETHER A CONFIGURED `environment` ENTRY ACTUALLY ARRIVED.
//
// Reported as a verdict, never as the value. The unit witnesses prove the
// plumbing in isolation; this lets a real journey prove it through the whole
// product path -- config file, declaration, registry, spawn -- and it is the
// only way to see that from the outside. The MATCH is reported rather than the
// contents, because `environment` is where an API token lives and a transcript
// that echoed one would leak exactly what this campaign is careful about.
const SENTINEL_OK =
  process.env.GAMMA_JOURNEY_SENTINEL === "configured-environment-reached-the-child";
const PROTOCOL_VERSION = "2024-11-05";

let initialized = false;
let handshaken = false;

// THE TRANSCRIPT. Screen text proves what a client DREW; it does not prove
// what crossed this process's stdio. Every inbound line and every outbound
// message is appended here with a monotonic ordinal and a timestamp, so a
// journey's claim about a tool call can be correlated against the actual
// request and response rather than inferred from a rendered row.
//
// Appended, never truncated: a transcript that rewrote itself on restart would
// destroy the evidence of the restart.
const fs = require("fs");
// ARGV FIRST, ENVIRONMENT SECOND -- and the order is forced, not stylistic.
// The frozen Mcp.LocalConfig declares an `environment` map for local servers,
// but the reconstruction's config parser reads only type/command/url and
// disabled/enabled: `environment`, `cwd`, `codemode` and `timeout` are
// SILENTLY DROPPED. So a transcript path passed by configuration environment
// would never reach this process, and the journey would quietly produce no
// transcript at all while appearing to work. The path is therefore an
// argument, which `command` does carry.
const TRANSCRIPT = process.argv[2] || process.env.GAMMA_MCP_TRANSCRIPT;
let ordinal = 0;
const record = (direction, payload) => {
  if (!TRANSCRIPT) return;
  ordinal += 1;
  try {
    fs.appendFileSync(
      TRANSCRIPT,
      JSON.stringify({ ordinal, at: new Date().toISOString(), pid: process.pid, direction, payload }) + "\n",
    );
  } catch {
    // A transcript failure must never change what the server answers: the
    // journey would then be measuring the logger.
  }
};

const send = (message) => {
  record("out", message);
  process.stdout.write(JSON.stringify(message) + "\n");
};
const fail = (id, code, message) => send({ jsonrpc: "2.0", id, error: { code, message } });

let buffer = "";
process.stdin.on("data", (chunk) => {
  buffer += chunk;
  let index;
  while ((index = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, index).trim();
    buffer = buffer.slice(index + 1);
    if (!line) continue;
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      record("in-unparseable", line.slice(0, 2048));
      continue;
    }
    record("in", message);

    if (message.method === "initialize") {
      handshaken = true;
      // Answers a revision OTHER than the one the product offers. The
      // pre-campaign exact-match handshake refused this outright, so a
      // successful connection here is itself the negotiation witness.
      send({
        jsonrpc: "2.0",
        id: message.id,
        result: {
          protocolVersion: PROTOCOL_VERSION,
          serverInfo: { name: "journey-server", version: "2.0.0" },
          capabilities: { tools: {} },
        },
      });
    } else if (message.method === "notifications/initialized") {
      // No reply, as the specification requires. Recorded, not ignored.
      initialized = handshaken;
    } else if (message.method === "tools/list") {
      if (!initialized) {
        fail(message.id, -32002, "tools/list before notifications/initialized");
        continue;
      }
      send({
        jsonrpc: "2.0",
        id: message.id,
        result: {
          tools: [
            {
              name: "journey_echo",
              description:
                "Returns a server-side token and a transform of its argument. " +
                "The answer cannot be produced without calling this server.",
              inputSchema: {
                type: "object",
                properties: { text: { type: "string" } },
                required: ["text"],
                additionalProperties: false,
              },
            },
          ],
        },
      });
    } else if (message.method === "tools/call") {
      if (!initialized) {
        fail(message.id, -32002, "tools/call before notifications/initialized");
        continue;
      }
      const name = message.params && message.params.name;
      if (name !== "journey_echo") {
        fail(message.id, -32602, `unknown tool ${name}`);
        continue;
      }
      const text = (message.params.arguments && message.params.arguments.text) || "";
      // The reversal is a transform of the caller's own input, so it binds THIS
      // call's arguments; the token binds this SERVER. Neither alone would do:
      // a token could be echoed from a cached earlier answer, and a reversal
      // could in principle be computed by the model.
      const reversed = [...text].reverse().join("");
      send({
        jsonrpc: "2.0",
        id: message.id,
        result: {
          content: [
            {
              type: "text",
              text:
                `${SERVER_TOKEN} reversed=${reversed} length=${text.length} ` +
                `sentinel=${SENTINEL_OK ? "reached" : "absent"}`,
            },
          ],
          isError: false,
        },
      });
    } else if (message.id !== undefined) {
      fail(message.id, -32601, "method not found");
    }
  }
});
