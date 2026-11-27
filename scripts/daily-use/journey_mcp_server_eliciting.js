// Bounded stdio MCP fixture for the retained global-form journey.
// Correlate its transcript with durable facts; this is not an external provider.
// The server token is never included in the user prompt.
const SERVER_TOKEN = "ZULUQUEBEC7731";

const SENTINEL_OK =
  process.env.GAMMA_JOURNEY_SENTINEL === "configured-environment-reached-the-child";
const PROTOCOL_VERSION = "2025-06-18";

let initialized = false;
let handshaken = false;
let pendingElicitation;

const fs = require("fs");
const TRANSCRIPT = process.argv[2] || process.env.GAMMA_MCP_TRANSCRIPT;
if (!TRANSCRIPT) throw new Error("journey transcript path is required");
let ordinal = 0;
const record = (direction, payload) => {
  if (ordinal >= 4096) throw new Error("journey transcript record bound");
  ordinal += 1;
  fs.appendFileSync(
    TRANSCRIPT,
    JSON.stringify({ ordinal, at: new Date().toISOString(), pid: process.pid, direction, payload }) + "\n",
  );
};

const send = (message) => {
  record("out", message);
  process.stdout.write(JSON.stringify(message) + "\n");
};
const fail = (id, code, message) => send({ jsonrpc: "2.0", id, error: { code, message } });

let buffer = "";
process.stdin.on("data", (chunk) => {
  if (chunk.length > 65536 - Buffer.byteLength(buffer)) {
    record("refusal", { reason: "input frame bound" });
    process.exit(2);
  }
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

    if (!message.method && message.id === 9001) {
      if (!pendingElicitation) {
        record("refusal", { reason: "elicitation response without live call" });
        continue;
      }
      const call = pendingElicitation;
      pendingElicitation = undefined;
      const confirmation = message.result?.content?.confirm;
      const accepted = message.result?.action === "accept" &&
        typeof confirmation === "string" && confirmation.length <= 256;
      const text = call.params.arguments.text;
      send({jsonrpc: "2.0", id: call.id, result: {
        content: [{type: "text", text: accepted
          ? `${SERVER_TOKEN} reversed=${[...text].reverse().join("")} length=${text.length} confirmation=${confirmation} sentinel=${SENTINEL_OK ? "reached" : "absent"}`
          : "elicitation was cancelled or refused; tool did not execute"}],
        isError: !accepted,
      }});
    } else if (message.method === "initialize") {
      if (!message.params?.capabilities?.elicitation?.form) {
        fail(message.id, -32602, "client did not advertise form elicitation");
        continue;
      }
      handshaken = true;
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
      if (!initialized || pendingElicitation || message.params?.name !== "journey_echo" ||
          typeof message.params?.arguments?.text !== "string" || message.params.arguments.text.length > 4096) {
        fail(message.id, -32602, "tool call is unavailable or invalid");
        continue;
      }
      pendingElicitation = message;
      record("out", { note: "sending elicitation/create before the tools/call reply" });
      send({
        jsonrpc: "2.0",
        id: 9001,
        method: "elicitation/create",
        params: {
          message: "journey_echo needs a confirmation before it runs",
          requestedSchema: {
            type: "object",
            properties: { confirm: { type: "string", maxLength: 256 } },
            required: ["confirm"],
          },
        },
      });
    } else if (message.id !== undefined) {
      fail(message.id, -32601, "method not found");
    }
  }
});
