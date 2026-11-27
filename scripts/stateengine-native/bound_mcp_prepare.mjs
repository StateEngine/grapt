// Real StateEngine issuance, config publication and CLI provisioning in leased roots.
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";

const engine = process.argv[2];
const moduleAt = path => import(pathToFileURL(`${engine}/${path}`));
const { registeredFixture } = await moduleAt("test/helpers/mcp-door-harness.mjs");
const { createAgent } = await moduleAt("src/agent-registry.mjs");
const { prepareSecodeLaunch } = await moduleAt("src/secode-launch-prep.mjs");
const { sendHumanMessage } = await moduleAt("src/human-message-send.mjs");
const { completeInboxIndexBuild } = await moduleAt("src/inbox-index-background.mjs");
const { readFloorMessageRecordsById } = await moduleAt("src/seat-messages.mjs");
const { startRemoteSeatMailDoor } = await moduleAt("src/remote-seat-mail-door.mjs");
const fixture = await registeredFixture("grapt-bound-mcp");
const project = "stateengine", agent = "restart-seat", recipient = "receiver-seat";
for (const id of [agent, recipient]) {
  await createAgent(fixture.root, project, {
    agentId: id, agentName: id, agentRole: "test-seat",
    permissions: ["Use this disposable fixture only"],
    ownerHumanId: fixture.ownerHumanId,
    actorType: "human", actorId: fixture.ownerHumanId,
  }, { registryRoot: fixture.registryRoot });
}
const emit = value => process.stdout.write(JSON.stringify(value) + "\n");
async function seed(count) {
  for (let n = 0; n < count; n++) {
    await sendHumanMessage(fixture.root, {
      project, to: agent, userId: fixture.ownerHumanId, kind: "done",
      body: `Disposable fixture inbox row ${n}: ${"x".repeat(240)}`,
    }, { registryRoot: fixture.registryRoot });
  }
  const index = await completeInboxIndexBuild(fixture.root, project, {
    additionalInboxIndexSeats: [agent, recipient],
  });
  if (index.state !== "ready") throw Error("fixture inbox index not ready");
}
await seed(1);
const door = await startRemoteSeatMailDoor({ root: fixture.root, project, targetProject: "secode", port: 0 });
await writeFile(join(fixture.root, ".stateengine", "engine-session.json"), JSON.stringify({
  organs: ["remote-seat-mail"],
  "remote-seat-mail": {
    command: process.execPath,
    args: [join(engine, "src/remote-seat-mail-door.mjs"), "--root", fixture.root,
      "--project", project, "--target-project", "secode", "--port", String(door.port)],
    healthUrl: `http://127.0.0.1:${door.port}/health`,
  },
}));
emit({ project, agent, recipient });
const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
try {
for await (const line of lines) {
  if (Buffer.byteLength(line) > 65536) throw Error("fixture command bound");
  const request = JSON.parse(line);
  if (request.action === "exit") break;
  if (request.action === "seed-large-inbox") {
    await seed(100);
    emit({ seeded: true });
  } else if (request.action === "verify-send") {
    const { records } = await readFloorMessageRecordsById(fixture.root, project, request.msg_id);
    const rows = records.filter(row => row.record_type === "message");
    emit({ verified: rows.length === 1 && rows[0].from_seat === agent
      && rows[0].to_seat === recipient && rows[0].body === "BOUND_REAL_STATEENGINE_SEND" });
  } else if (request.action === "launch") {
    const launched = await prepareSecodeLaunch({
      rootDir: fixture.root, project, seatId: agent, executionRoot: fixture.root,
      seat: { surface: "grapt", command: request.cli, grapt_tui_binary: request.cli,
        grapt_service: request.service, grapt_agent: "agt_live_headless1", model: "openai/gpt-live" },
      spawnEnv: { ...process.env, STATEENGINE_REGISTRY_ROOT: fixture.registryRoot },
      dependencies: {
        mintSecodeSessionId: () => request.session,
        // A small labeled packet keeps this test focused on dispatch. Its delivery,
        // acknowledgement, issued capability and provision call are all real.
        buildContextPacket: async () => ({ packet: "# Disposable bound MCP fixture\npacket_id: "
          + "a".repeat(64) + "\nRead, list and send only inside this private fixture.\n" }),
      },
    });
    emit({ receipt: launched.receipt });
  } else {
    throw Error("unknown fixture action");
  }
}
} finally {
  await door.stop();
}
