import { AsyncLocalStorage } from "node:async_hooks"
import { createHash } from "node:crypto"
import { readFileSync } from "node:fs"
import { builtinModules, registerHooks } from "node:module"
import path from "node:path"
import readline from "node:readline"
import { fileURLToPath, pathToFileURL } from "node:url"
import { format as formatValue } from "node:util"

const PROTOCOL_VERSION = 2

// The protocol owns stdout. Guest console output and stray stdout writes go to
// stderr, which the supervisor discards, before any guest module can load.
const writeProtocol = process.stdout.write.bind(process.stdout)
const writeDiagnostic = process.stderr.write.bind(process.stderr)
process.stdout.write = (chunk, encoding, callback) => writeDiagnostic(chunk, encoding, callback)
for (const method of ["log", "info", "debug", "warn", "error", "trace", "dir", "dirxml", "table"]) {
  console[method] = (...values) => {
    try { writeDiagnostic(formatValue(...values) + "\n") } catch {}
  }
}

const authority = new AsyncLocalStorage()
const pins = new Map()
const foldedPins = new Map()
const callbacks = new Map()
const hostRequests = new Map()
const aborts = new Map()
let limits = { frame_bytes: 1024 * 1024, registrations: 128, pending_responses: 8 }
let networkRequests = 0
let pendingNetwork = 0
let generation
let root
let entrypoint
let loadedPlugin
let plugin
let callbackSequence = 1
let hostSequence = 1

function encodeFrame(value) {
  const encoded = JSON.stringify(value)
  if (Buffer.byteLength(encoded) + 1 > limits.frame_bytes) return undefined
  return encoded + "\n"
}

function response(request, outcome, payload = null, error = undefined) {
  const value = {
    version: PROTOCOL_VERSION,
    generation: request.generation,
    correlation: request.correlation,
    outcome,
    payload,
  }
  if (error !== undefined) value.error = String(error).slice(0, 4096)
  const frame = encodeFrame(value) ?? encodeFrame({
    ...value, outcome: "error", payload: null, error: "bridge response exceeds the frame bound",
  })
  writeProtocol(frame)
}

function hostCall(operation, payload = {}) {
  const correlation = hostSequence++
  const frame = encodeFrame({
    kind: "host-request",
    version: PROTOCOL_VERSION,
    generation,
    correlation,
    operation,
    payload,
    authority: authority.getStore()?.authority ?? null,
  })
  if (frame === undefined) return Promise.reject(new Error("plugin host request exceeds the frame bound"))
  return new Promise((resolve, reject) => {
    hostRequests.set(correlation, { resolve, reject })
    writeProtocol(frame)
  })
}

// HTTP(S) always crosses the host's declaration-bound broker. Native socket
// APIs are denied below and by the OS, including imports through dependencies.
const requestType = Request
const responseType = Response
const fetchBodyLimit = () => Math.min(128 * 1024, Math.floor(limits.frame_bytes / 8))

async function boundedRequestBody(request) {
  if (request.body === null) return null
  const reader = request.body.getReader()
  const chunks = []
  let size = 0
  try {
    for (;;) {
      const { value, done } = await reader.read()
      if (done) break
      size += value.byteLength
      if (size > fetchBodyLimit()) throw new Error("plugin_network_body: request body exceeds the broker bound")
      chunks.push(Buffer.from(value))
    }
  } finally {
    await reader.cancel().catch(() => {})
    reader.releaseLock()
  }
  return Buffer.concat(chunks, size).toString("base64")
}

function abortableNetwork(pending, signal) {
  return new Promise((resolve, reject) => {
    const abort = () => reject(signal.reason)
    signal.addEventListener("abort", abort, { once: true })
    if (signal.aborted) abort()
    pending.then(resolve, reject).finally(() => signal.removeEventListener("abort", abort))
  })
}

async function brokerFetch(input, init) {
  const original = new requestType(input, init)
  original.signal.throwIfAborted()
  const body = await boundedRequestBody(original)
  let url = original.url
  let method = original.method
  let encodedBody = body
  const headers = new Headers(original.headers)
  for (let hop = 0; hop <= 10; hop++) {
    original.signal.throwIfAborted()
    if (pendingNetwork >= limits.pending_responses) {
      throw new Error("plugin_network_busy: the bounded fetch queue is full; wait for an outstanding request to finish")
    }
    pendingNetwork++
    const pending = hostCall("network-fetch", {
      url, method, headers: [...headers], body: encodedBody,
    }).finally(() => { pendingNetwork-- })
    const result = await abortableNetwork(pending, original.signal)
    original.signal.throwIfAborted()
    const location = new Headers(result.headers).get("location")
    if ([301, 302, 303, 307, 308].includes(result.status) && location !== null) {
      if (original.redirect === "error") throw new Error("plugin_network_redirect: redirects are disabled for this request")
      if (original.redirect !== "manual") {
        if (hop === 10) throw new Error("plugin_network_redirect: exceeded 10 redirects")
        const next = new URL(location, url)
        if (next.origin !== new URL(url).origin) {
          headers.delete("authorization")
          headers.delete("cookie")
        }
        if ((result.status === 303 && method !== "HEAD") || ([301, 302].includes(result.status) && method === "POST")) {
          method = "GET"
          encodedBody = null
          headers.delete("content-type")
        }
        url = next.href
        continue
      }
    }
    const response = new responseType(
      method === "HEAD" || [204, 205, 304].includes(result.status) ? null : Buffer.from(result.body, "base64"),
      { status: result.status, headers: result.headers },
    )
    Object.defineProperties(response, {
      url: { value: url }, redirected: { value: hop > 0 },
    })
    return response
  }
  throw new Error("plugin_network_redirect: exceeded redirect bound")
}
async function containedFetch(input, init) {
  if (networkRequests >= limits.pending_responses || pendingNetwork >= limits.pending_responses) {
    throw new Error("plugin_network_busy: the bounded fetch queue is full; wait for an outstanding request to finish")
  }
  networkRequests++
  try { return await brokerFetch(input, init) }
  finally { networkRequests-- }
}
Object.defineProperty(globalThis, "fetch", { value: containedFetch, writable: false, configurable: false })
for (const name of ["WebSocket", "EventSource"]) {
  if (!(name in globalThis)) continue
  Object.defineProperty(globalThis, name, {
    value: function () {
      throw new Error(`plugin_network_api_denied: ${name} is unavailable; use fetch and declare the HTTP(S) origin in this plugin's permissions.network`)
    },
    writable: false,
    configurable: false,
  })
}

function diagnostic(error) {
  const message = error instanceof Error ? error.message : String(error)
  if (["ERR_ACCESS_DENIED", "EACCES", "EPERM"].includes(error?.code)) {
    return `plugin_filesystem_denied: ${message}; declare the required workspace path in this plugin's permissions.read or permissions.write in user-global or launch-explicit configuration`
  }
  return message
}

// ---------------------------------------------------------------------------
// Pinned module loading. Every module and package.json read is verified
// against the admitted snapshot; nothing outside it can resolve or load.

const DENIED_BUILTINS = new Set([
  "child_process", "cluster", "inspector", "inspector/promises", "module", "repl", "trace_events",
  "v8", "vm", "wasi", "worker_threads", "sqlite", "test", "test/reporters",
  "net", "http", "http2", "https", "tls", "dgram", "dns", "dns/promises",
])
const LOADABLE = [".js", ".mjs", ".cjs", ".json"]
const packageCache = new Map()

function builtinName(specifier) {
  const name = specifier.startsWith("node:") ? specifier.slice(5) : specifier
  return builtinModules.includes(name) || specifier.startsWith("node:") ? name : undefined
}

function allowedBuiltin(specifier) {
  const name = builtinName(specifier)
  if (name === undefined) return undefined
  if (DENIED_BUILTINS.has(name) || !builtinModules.includes(name)) {
    throw new Error(`plugin_builtin_denied: denied built-in ${specifier}; use fetch with an exact permissions.network HTTP(S) origin; process and runtime escape APIs are unavailable`)
  }
  return `node:${name}`
}

const realGetBuiltinModule = process.getBuiltinModule
process.getBuiltinModule = (id) => {
  const url = allowedBuiltin(String(id))
  if (url === undefined) throw new Error(`plugin requested unknown built-in ${id}`)
  return realGetBuiltinModule(url)
}

function pinKey(relative) {
  return process.platform === "win32" ? relative.toLowerCase() : relative
}

function pinFor(relative) {
  return pins.has(relative) ? relative : foldedPins.get(pinKey(relative))
}

function relativeTo(absolute) {
  const relative = path.relative(root, absolute)
  if (relative === "" || relative.startsWith("..") || path.isAbsolute(relative)) return undefined
  return relative.split(path.sep).join("/")
}

function readPinned(relative) {
  const key = pinFor(relative)
  if (key === undefined) throw new Error(`plugin module ${relative} is not part of the admitted snapshot`)
  const pin = pins.get(key)
  const bytes = readFileSync(path.join(root, ...key.split("/")))
  const actual = createHash("sha256").update(bytes).digest("hex")
  if (bytes.length !== pin.bytes || actual !== pin.sha256) {
    throw new Error(`plugin module ${key} changed after trust validation`)
  }
  return bytes
}

function packageJson(directory) {
  const relative = directory === "" ? "package.json" : `${directory}/package.json`
  if (pinFor(relative) === undefined) return undefined
  if (!packageCache.has(relative)) {
    try {
      packageCache.set(relative, JSON.parse(readPinned(relative).toString("utf8")))
    } catch (error) {
      throw new Error(`plugin package manifest ${relative} is invalid: ${error.message}`)
    }
  }
  return packageCache.get(relative)
}

function joinRelative(base, addition) {
  const parts = []
  for (const part of `${base}/${addition}`.split("/")) {
    if (part === "" || part === ".") continue
    if (part === "..") {
      if (parts.length === 0) return undefined
      parts.pop()
    } else {
      parts.push(part)
    }
  }
  return parts.join("/")
}

function resolveFile(relative) {
  if (relative === undefined) return undefined
  if (pinFor(relative) !== undefined && LOADABLE.includes(path.posix.extname(relative))) return pinFor(relative)
  for (const extension of LOADABLE) {
    if (pinFor(relative + extension) !== undefined) return pinFor(relative + extension)
  }
  const manifest = packageJson(relative)
  if (typeof manifest?.main === "string") {
    const main = resolveFile(joinRelative(relative, manifest.main))
    if (main !== undefined) return main
  }
  for (const extension of LOADABLE) {
    const index = relative === "" ? `index${extension}` : `${relative}/index${extension}`
    if (pinFor(index) !== undefined) return pinFor(index)
  }
  return undefined
}

function exportTarget(target, conditions, substitution) {
  if (typeof target === "string") {
    if (!target.startsWith("./")) return undefined
    return substitution === undefined ? target : target.split("*").join(substitution)
  }
  if (Array.isArray(target)) {
    for (const item of target) {
      const resolved = exportTarget(item, conditions, substitution)
      if (resolved !== undefined) return resolved
    }
    return undefined
  }
  if (target && typeof target === "object") {
    for (const [condition, value] of Object.entries(target)) {
      if (condition === "default" || conditions.includes(condition)) {
        const resolved = exportTarget(value, conditions, substitution)
        if (resolved !== undefined) return resolved
      }
    }
  }
  return undefined
}

function resolveExports(directory, exports, subpath, conditions) {
  const map = typeof exports === "string" || Array.isArray(exports) ||
    (exports && typeof exports === "object" && !Object.keys(exports).some((key) => key.startsWith(".")))
    ? { ".": exports }
    : exports
  if (Object.hasOwn(map, subpath)) return exportTarget(map[subpath], conditions)
  let best
  for (const key of Object.keys(map)) {
    const star = key.indexOf("*")
    if (star < 0) continue
    const prefix = key.slice(0, star)
    const suffix = key.slice(star + 1)
    if (subpath.startsWith(prefix) && subpath.endsWith(suffix) && subpath.length >= key.length - 1 &&
        (best === undefined || prefix.length > best.prefix.length)) {
      best = { key, prefix, substitution: subpath.slice(prefix.length, subpath.length - suffix.length) }
    }
  }
  if (best === undefined) return undefined
  return exportTarget(map[best.key], conditions, best.substitution)
}

function resolveBare(specifier, parentDirectory, conditions) {
  const parts = specifier.split("/")
  const nameLength = specifier.startsWith("@") ? 2 : 1
  if (parts.length < nameLength || parts.slice(0, nameLength).some((part) => part === "" || part === "." || part === "..")) {
    return undefined
  }
  const name = parts.slice(0, nameLength).join("/")
  const rest = parts.slice(nameLength).join("/")
  let directory = parentDirectory
  for (;;) {
    const candidate = directory === "" ? `node_modules/${name}` : `${directory}/node_modules/${name}`
    const manifest = packageJson(candidate)
    if (manifest !== undefined || resolveFile(candidate) !== undefined) {
      if (manifest?.exports !== undefined) {
        const target = resolveExports(candidate, manifest.exports, rest === "" ? "." : `./${rest}`, conditions)
        return target === undefined ? undefined : resolveFile(joinRelative(candidate, target))
      }
      return resolveFile(rest === "" ? candidate : joinRelative(candidate, rest))
    }
    if (directory === "") return undefined
    directory = directory.includes("/") ? directory.slice(0, directory.lastIndexOf("/")) : ""
  }
}

function resolvePinned(specifier, parentRelative, conditions) {
  const builtin = allowedBuiltin(specifier)
  if (builtin !== undefined) return builtin
  const parentDirectory = parentRelative.includes("/") ? parentRelative.slice(0, parentRelative.lastIndexOf("/")) : ""
  let relative
  if (specifier.startsWith("file:")) {
    relative = resolveFile(relativeTo(fileURLToPath(specifier)))
  } else if (specifier.startsWith("./") || specifier.startsWith("../") || specifier === "." || specifier === "..") {
    relative = resolveFile(joinRelative(parentDirectory, specifier))
  } else if (path.isAbsolute(specifier)) {
    relative = resolveFile(relativeTo(specifier))
  } else if (/^[a-z][a-z0-9+.-]*:/i.test(specifier)) {
    throw new Error(`plugin import scheme is not admitted: ${specifier.slice(0, 64)}`)
  } else {
    relative = resolveBare(specifier, parentDirectory, conditions)
  }
  if (relative === undefined) {
    throw new Error(
      `plugin ${loadedPlugin ?? "module"} import ${JSON.stringify(specifier).slice(0, 256)} does not resolve inside its admitted snapshot; vendor dependencies under the plugin directory as regular files`,
    )
  }
  return pathToFileURL(path.join(root, ...relative.split("/"))).href
}

function moduleFormat(relative) {
  const extension = path.posix.extname(relative)
  if (extension === ".mjs") return "module"
  if (extension === ".cjs") return "commonjs"
  if (extension === ".json") return "json"
  let directory = relative.includes("/") ? relative.slice(0, relative.lastIndexOf("/")) : ""
  for (;;) {
    const manifest = packageJson(directory)
    if (manifest !== undefined) return manifest.type === "module" ? "module" : "commonjs"
    if (directory === "") return "commonjs"
    directory = directory.includes("/") ? directory.slice(0, directory.lastIndexOf("/")) : ""
  }
}

function installLoader() {
  registerHooks({
    resolve(specifier, context, nextResolve) {
      if (root === undefined) return nextResolve(specifier, context)
      const parent = context.parentURL?.startsWith("file:") ? relativeTo(fileURLToPath(context.parentURL)) : undefined
      if (parent === undefined && allowedBuiltin(specifier) === undefined && !specifier.startsWith("file:")) {
        throw new Error(`plugin import ${specifier.slice(0, 128)} has no admitted parent module`)
      }
      const conditions = context.conditions ?? ["node", "import"]
      return { url: resolvePinned(specifier, parent ?? entrypoint, conditions), shortCircuit: true }
    },
    load(url, context, nextLoad) {
      if (url.startsWith("node:")) {
        allowedBuiltin(url)
        return nextLoad(url, context)
      }
      if (!url.startsWith("file:")) throw new Error(`plugin module URL is not admitted: ${url.slice(0, 64)}`)
      const relative = relativeTo(fileURLToPath(url))
      if (relative === undefined) throw new Error("plugin module is outside its admitted snapshot")
      return { format: moduleFormat(relative), source: readPinned(relative), shortCircuit: true }
    },
  })
}

// ---------------------------------------------------------------------------
// Plugin context.

function callback(fn, effect, domain = undefined, name = undefined) {
  if (callbacks.size >= limits.registrations) throw new Error("plugin callback registrations exceed the bound")
  const id = callbackSequence++
  callbacks.set(id, { fn, effect, domain, name })
  return id
}

function registration(dispose = () => {}) {
  return { dispose: async () => dispose() }
}

function retainedCallback(registrations, domain, name, fn, effect) {
  const id = callback(fn, effect, domain, name)
  const row = { domain, name, callback: id }
  registrations.callbacks.push(row)
  return registration(() => {
    callbacks.delete(id)
    const index = registrations.callbacks.indexOf(row)
    if (index >= 0) registrations.callbacks.splice(index, 1)
  })
}

function requiredEnvelope(value, domain) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${domain}.transform received a malformed host draft`)
  }
  return value
}

function requiredArray(value, key, domain) {
  const items = requiredEnvelope(value, domain)[key]
  if (!Array.isArray(items)) throw new Error(`${domain}.transform host draft omitted ${key}`)
  return items
}

function keyedDraft(items, key) {
  const get = (id) => items.find((item) => item?.[key] === id)
  return {
    list: () => items,
    get,
    update: (id, update) => {
      const item = get(id)
      if (item !== undefined) update(item)
    },
    remove: (id) => {
      const index = items.findIndex((item) => item?.[key] === id)
      if (index >= 0) items.splice(index, 1)
    },
  }
}

function transformDraft(domain, value, effect) {
  const envelope = requiredEnvelope(value, domain)
  const owner = loadedPlugin
  if (domain === "agent") {
    const draft = keyedDraft(requiredArray(envelope, "items", domain), "id")
    return { ...draft, default: (id) => { envelope.default = id ?? null } }
  }
  if (domain === "catalog") {
    const providers = requiredArray(envelope, "providers", domain)
    const record = (id) => providers.find((item) => item?.provider?.id === id)
    const model = (providerID, modelID) => record(providerID)?.models?.find((item) => item?.id === modelID)
    return {
      provider: {
        list: () => providers,
        get: record,
        update: (id, update) => { const item = record(id); if (item) update(item.provider) },
        remove: (id) => { const index = providers.findIndex((item) => item?.provider?.id === id); if (index >= 0) providers.splice(index, 1) },
      },
      model: {
        get: model,
        update: (providerID, modelID, update) => { const item = model(providerID, modelID); if (item) update(item) },
        remove: (providerID, modelID) => {
          const models = record(providerID)?.models
          if (!Array.isArray(models)) return
          const index = models.findIndex((item) => item?.id === modelID)
          if (index >= 0) models.splice(index, 1)
        },
        default: {
          get: () => envelope.default ?? undefined,
          set: (providerID, modelID) => { envelope.default = { providerID, modelID } },
        },
      },
    }
  }
  if (domain === "command") {
    const items = requiredArray(envelope, "items", domain)
    const draft = keyedDraft(items, "name")
    return {
      ...draft,
      update: (name, update) => {
        let item = draft.get(name)
        if (item === undefined) {
          item = { name, template: "" }
          items.push(item)
        }
        update(item)
        item.name = name
      },
    }
  }
  if (domain === "integration") {
    const draft = keyedDraft(requiredArray(envelope, "items", domain), "id")
    const methods = requiredArray(envelope, "methods", domain)
    return {
      ...draft,
      method: {
        list: (integrationID) => methods.filter((item) => item?.integrationID === integrationID).map((item) => item.method),
        update: (input) => {
          if (!input || typeof input !== "object" || typeof input.integrationID !== "string" || !input.method) {
            throw new Error("integration.transform method registration is malformed")
          }
          const row = { integrationID: input.integrationID, method: input.method }
          if (typeof input.authorize === "function") row.authorize = { plugin_id: owner, callback: callback(input.authorize, effect, "integration", "authorize") }
          if (typeof input.refresh === "function") row.refresh = { plugin_id: owner, callback: callback(input.refresh, effect, "integration", "refresh") }
          if (typeof input.label === "function") row.label = { plugin_id: owner, callback: callback(input.label, effect, "integration", "label") }
          const index = methods.findIndex((item) => item?.integrationID === input.integrationID && item?.method?.id === input.method.id)
          if (index >= 0) methods[index] = row
          else methods.push(row)
        },
        remove: (integrationID, method) => {
          const index = methods.findIndex((item) => item?.integrationID === integrationID && item?.method?.id === method?.id)
          if (index >= 0) methods.splice(index, 1)
        },
      },
    }
  }
  if (domain === "reference") {
    const items = requiredArray(envelope, "items", domain)
    return {
      add: (name, source) => {
        const index = items.findIndex((item) => Array.isArray(item) && item[0] === name)
        if (index >= 0) items[index] = [name, source]
        else items.push([name, source])
      },
      remove: (name) => { const index = items.findIndex((item) => Array.isArray(item) && item[0] === name); if (index >= 0) items.splice(index, 1) },
      list: () => items,
    }
  }
  if (domain === "skill") {
    const sources = requiredArray(envelope, "sources", domain)
    return { source: (source) => { sources.push(source) }, list: () => sources }
  }
  if (domain === "websearch") {
    const providers = requiredArray(envelope, "providers", domain)
    return {
      add: (definition) => {
        if (!definition || typeof definition.id !== "string" || typeof definition.name !== "string" || typeof definition.execute !== "function") {
          throw new Error("websearch.transform provider registration is malformed")
        }
        const item = { id: definition.id, name: definition.name, plugin_id: owner, callback: callback(definition.execute, effect, "websearch", "execute") }
        const index = providers.findIndex((provider) => provider?.id === definition.id)
        if (index >= 0) providers[index] = item
        else providers.push(item)
      },
      default: {
        get: () => envelope.default ?? undefined,
        set: (providerID) => { envelope.default = providerID },
      },
    }
  }
  throw new Error(`plugin transform domain ${domain} is unsupported`)
}

function transformDomain(registrations, name, effect) {
  return {
    transform: async (fn) => {
      registrations.capabilities.push(`${name}.transform`)
      return retainedCallback(registrations, name, "transform", fn, effect)
    },
  }
}

function hookDomain(registrations, domain, effect) {
  return async (name, fn) => {
    registrations.capabilities.push(`${domain}.hook.${name}`)
    return retainedCallback(registrations, domain, `hook.${name}`, fn, effect)
  }
}

function unsupportedAisdkHook(owner) {
  return async (name) => {
    const surface = `aisdk.hook.${name}`
    const decision = "grapt.plugin-aisdk-replacement-exclusion-2026-09-16"
    throw new Error(
      `plugin ${owner} requested unsupported compatibility surface ${surface}: arbitrary executable ` +
      `AI SDK or LanguageModel replacement is excluded by ${decision}; compiled provider composition ` +
      "and ModelRegistry issuance remain authoritative",
    )
  }
}

function eventDomain() {
  return {
    subscribe() {
      let subscription
      let opening
      let closed = false
      const open = async () => {
        if (subscription !== undefined) return subscription
        opening ??= hostCall("event-subscribe", { action: "open" })
        const result = await opening
        if (!Number.isSafeInteger(result?.subscription) || result.subscription < 1) {
          throw new Error("event.subscribe host returned an invalid subscription")
        }
        subscription = result.subscription
        return subscription
      }
      return {
        [Symbol.asyncIterator]() { return this },
        async next() {
          if (closed) return { done: true, value: undefined }
          const id = await open()
          const result = await hostCall("event-subscribe", { action: "next", subscription: id })
          if (!result || typeof result.done !== "boolean") {
            throw new Error("event.subscribe host returned a malformed item")
          }
          if (result.done) closed = true
          return { done: result.done, value: result.value }
        },
        async return() {
          if (closed) return { done: true, value: undefined }
          closed = true
          const id = await open()
          await hostCall("event-subscribe", { action: "cancel", subscription: id })
          return { done: true, value: undefined }
        },
      }
    },
  }
}

function promiseContext(declaration, registrations, app, effect) {
  const call = (operation) => (payload = {}) => hostCall(operation, payload)
  const tool = {
    transform: async (fn) => {
      const draft = {
        add(definition) {
          if (!definition || typeof definition !== "object" || typeof definition.name !== "string" || typeof definition.execute !== "function") {
            throw new Error("plugin tool registration is malformed")
          }
          const id = callback(definition.execute, effect, "tool", "execute")
          registrations.tools.push({
            name: definition.name,
            description: typeof definition.description === "string" ? definition.description : null,
            input_schema: definition.args ?? definition.input ?? { type: "object", additionalProperties: true },
            callback: id,
          })
        },
      }
      await fn(draft)
      registrations.capabilities.push("tool.transform")
      return registration()
    },
    hook: async (name, fn) => hookDomain(registrations, "tool", effect)(name, fn),
  }
  const generic = (name, methods = []) => Object.assign(transformDomain(registrations, name, effect),
    Object.fromEntries(methods.map((method) => [method, call(`${name}-${method}`)])))
  return {
    app,
    options: declaration.options ?? {},
    agent: generic("agent", ["get", "list", "reload"]),
    aisdk: { hook: unsupportedAisdkHook(loadedPlugin) },
    catalog: {
      ...generic("catalog", ["reload"]),
      provider: { list: call("catalog-provider-list"), get: call("catalog-provider-get") },
      model: { list: call("catalog-model-list"), default: call("catalog-model-default") },
    },
    command: generic("command", ["list", "reload"]),
    event: eventDomain(),
    integration: Object.assign(generic("integration", ["list", "get", "reload"]), {
      connect: { key: call("integration-connect-key") },
      oauth: {
        connect: call("integration-oauth-connect"), status: call("integration-oauth-status"),
        complete: call("integration-oauth-complete"), cancel: call("integration-oauth-cancel"),
      },
      command: {
        connect: call("integration-command-connect"), status: call("integration-command-status"),
        cancel: call("integration-command-cancel"),
      },
      connection: {
        active: (id) => hostCall("integration-connection-active", { id }),
        resolve: (connection) => hostCall("integration-connection-resolve", { connection }),
      },
    }),
    plugin: { list: call("plugin-list") },
    reference: generic("reference", ["list", "reload"]),
    session: {
      hook: hookDomain(registrations, "session", effect), create: call("session-create"),
      get: call("session-get"), prompt: call("session-prompt"),
      generate: call("session-generate"), command: call("session-command"),
      synthetic: call("session-synthetic"), interrupt: call("session-interrupt"),
    },
    skill: generic("skill", ["list", "reload"]),
    tool,
    websearch: Object.assign(generic("websearch", ["providers", "reload"]), {
      query: call("websearch-query"),
      selection: { get: call("websearch-selection-get"), put: call("websearch-selection-put"), update: call("websearch-selection-put") },
    }),
  }
}

function effectContext(value, api) {
  const { Effect } = api
  const lift = (fn) => (...args) => Effect.tryPromise({
    try: () => Promise.resolve(fn(...args)),
    catch: (error) => error,
  })
  const walk = (input, key = "") => {
    if (typeof input === "function") {
      const lifted = lift(input)
      if (key !== "transform" && key !== "hook") return lifted
      return (...args) => Effect.map(lifted(...args), (registered) => ({
        dispose: lift(registered.dispose)(),
      }))
    }
    if (!input || typeof input !== "object" || Array.isArray(input)) return input
    return Object.fromEntries(Object.entries(input).map(([name, item]) => [name, walk(item, name)]))
  }
  return {
    app: value.app,
    options: value.options,
    ...Object.fromEntries(
      Object.entries(value)
        .filter(([name]) => name !== "app" && name !== "options")
        .map(([name, item]) => [name, walk(item, name)]),
    ),
  }
}

async function effectApi() {
  const conditions = ["node", "import"]
  const [Effect, Scope, Exit] = await Promise.all(
    ["effect/Effect", "effect/Scope", "effect/Exit"].map((name) => import(resolvePinned(name, entrypoint, conditions))),
  )
  return { Effect, Scope, Exit }
}

async function settle(value, api) {
  if (api?.Effect?.isEffect(value)) return api.Effect.runPromise(value)
  return value
}

// ---------------------------------------------------------------------------
// Operations.

async function load(payload) {
  if (root !== undefined) throw new Error("plugin bridge already loaded a plugin")
  if (typeof payload?.root !== "string" || typeof payload?.entrypoint !== "string") {
    throw new Error("plugin load request is malformed")
  }
  if (Number.isSafeInteger(payload.limits?.frame_bytes) && Number.isSafeInteger(payload.limits?.registrations)) {
    limits = { frame_bytes: payload.limits.frame_bytes, registrations: payload.limits.registrations, pending_responses: Math.max(1, Math.min(8, payload.limits.pending_responses)) }
  } else {
    throw new Error("plugin load request omitted its bridge limits")
  }
  if (pinFor(payload.entrypoint) === undefined) throw new Error("plugin entrypoint is not pinned")
  root = payload.root
  entrypoint = payload.entrypoint
  installLoader()
  const module = await import(pathToFileURL(path.join(root, ...entrypoint.split("/"))).href)
  const definition = module.default ?? module.plugin
  if (!definition || typeof definition.id !== "string" || definition.id === "" ||
      (typeof definition.setup !== "function" && typeof definition.effect !== "function")) {
    throw new Error("plugin module does not export a supported plugin definition")
  }
  loadedPlugin = definition.id
  plugin = { definition }
  return { plugin_id: definition.id }
}

async function activate(payload) {
  if (plugin === undefined || plugin.registrations !== undefined) throw new Error("plugin activation is out of order")
  const { definition } = plugin
  const registrations = { plugin_id: definition.id, tools: [], callbacks: [], capabilities: [] }
  let cleanup
  let effect
  try {
    if (typeof definition.effect === "function") {
      effect = await effectApi()
      const scope = await effect.Effect.runPromise(effect.Scope.make())
      try {
        const host = effectContext(promiseContext(payload.plugin, registrations, payload.app, effect), effect)
        await effect.Effect.runPromise(effect.Effect.provideService(definition.effect(host), effect.Scope.Scope, scope))
        cleanup = () => effect.Effect.runPromise(effect.Scope.close(scope, effect.Exit.void))
      } catch (error) {
        await effect.Effect.runPromise(effect.Scope.close(scope, effect.Exit.void))
        throw error
      }
    } else {
      cleanup = await definition.setup(promiseContext(payload.plugin, registrations, payload.app, undefined))
    }
    if (cleanup !== undefined && typeof cleanup !== "function") throw new Error("plugin setup returned an invalid cleanup value")
  } catch (error) {
    callbacks.clear()
    throw error
  }
  plugin.cleanup = cleanup
  plugin.registrations = registrations
  return registrations
}

function withCall(request, run) {
  const call = typeof request.payload?.call === "string" ? request.payload.call : undefined
  const controller = new AbortController()
  if (call !== undefined) aborts.set(call, controller)
  const store = { authority: Number.isSafeInteger(request.payload?.authority) ? request.payload.authority : null }
  return authority.run(store, () => run(controller.signal)).finally(() => {
    if (call !== undefined && aborts.get(call) === controller) aborts.delete(call)
  })
}

async function invokeCallback(payload, signal) {
  const item = callbacks.get(payload?.callback)
  if (!item) throw new Error("plugin callback is absent")
  const value = payload.value
  if (item.name === "transform") {
    await settle(item.fn(transformDraft(item.domain, value, item.effect)), item.effect)
    return value ?? null
  }
  if (item.name === "authorize") {
    const authorization = await settle(item.fn(value ?? {}), item.effect)
    if (!authorization || typeof authorization !== "object" || !["code", "auto"].includes(authorization.mode)) {
      throw new Error("integration OAuth authorize callback returned a malformed authorization")
    }
    const forwarded = {
      mode: authorization.mode,
      url: typeof authorization.url === "string" ? authorization.url : null,
      instructions: typeof authorization.instructions === "string" ? authorization.instructions : null,
    }
    if (Number.isSafeInteger(authorization.expiresAt)) forwarded.expiresAt = authorization.expiresAt
    if (authorization.mode === "code") {
      if (typeof authorization.callback !== "function") throw new Error("code OAuth authorization omitted its credential callback")
      forwarded.credentialCallback = callback(authorization.callback, item.effect, "integration", "credential")
    } else {
      forwarded.credential = await settle(authorization.callback, item.effect)
    }
    return forwarded
  }
  if (item.name === "credential") {
    try {
      return (await settle(item.fn(value), item.effect)) ?? null
    } finally {
      callbacks.delete(payload.callback)
    }
  }
  if (["execute", "refresh", "label"].includes(item.name)) {
    return (await settle(item.fn(value, { signal }), item.effect)) ?? null
  }
  await settle(item.fn(value), item.effect)
  return value ?? null
}

async function cleanupAll() {
  let failure
  try {
    if (plugin?.cleanup) await plugin.cleanup()
  } catch (error) {
    failure = error
  }
  callbacks.clear()
  for (const controller of aborts.values()) controller.abort()
  aborts.clear()
  for (const pending of hostRequests.values()) pending.reject(new Error("plugin bridge is closing"))
  hostRequests.clear()
  if (failure !== undefined) throw new Error(`plugin ${loadedPlugin} cleanup failed: ${String(failure).slice(0, 512)}`)
}

async function handle(request) {
  if (request.version !== PROTOCOL_VERSION) throw new Error("plugin bridge protocol version mismatch")
  if (!Number.isSafeInteger(request.generation) || request.generation < 1) throw new Error("plugin generation is invalid")
  if (generation !== undefined && request.generation !== generation) throw new Error("stale plugin generation request")
  generation = request.generation
  switch (request.operation) {
    case "pin-files": {
      if (root !== undefined) throw new Error("plugin source pins are sealed after load")
      const files = request.payload?.files
      if (!Array.isArray(files)) throw new Error("plugin source pin batch is malformed")
      for (const row of files) {
        if (!Array.isArray(row) || typeof row[0] !== "string" || !/^[0-9a-f]{64}$/.test(row[1]) || !Number.isSafeInteger(row[2])) {
          throw new Error("plugin source pin row is malformed")
        }
        pins.set(row[0], { sha256: row[1], bytes: row[2] })
        foldedPins.set(pinKey(row[0]), row[0])
      }
      return { pinned: pins.size }
    }
    case "load":
      return load(request.payload)
    case "activate":
      return activate(request.payload)
    case "invoke-tool":
      return withCall(request, async (signal) => {
        const item = callbacks.get(request.payload?.callback)
        if (!item || item.domain !== "tool") throw new Error("plugin tool callback is absent")
        const result = await settle(item.fn(request.payload.input, {
          ...(request.payload.context ?? {}),
          abort: signal,
          progress: async () => {},
        }), item.effect)
        return result ?? null
      })
    case "invoke-callback":
      return withCall(request, (signal) => invokeCallback(request.payload, signal))
    case "cancel-call": {
      const controller = aborts.get(request.payload?.call)
      if (controller !== undefined) controller.abort(new Error("plugin call was cancelled by the host"))
      return { cancelled: controller !== undefined }
    }
    case "cleanup":
      await cleanupAll()
      return { cleaned: true }
    default:
      throw new Error("unknown plugin bridge operation")
  }
}

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
lines.on("line", (line) => {
  if (Buffer.byteLength(line) + 1 > limits.frame_bytes) {
    process.exitCode = 64
    lines.close()
    return
  }
  let request
  try {
    request = JSON.parse(line)
    if (request.operation === "host-response") {
      const pending = hostRequests.get(request.correlation)
      if (!pending) throw new Error("plugin host response correlation is unknown")
      hostRequests.delete(request.correlation)
      if (request.payload?.outcome === "ok") pending.resolve(request.payload.payload)
      else pending.reject(new Error(request.payload?.error ?? "plugin host operation failed"))
      return
    }
    void handle(request).then((payload) => {
      response(request, "ok", payload)
      if (request.operation === "cleanup") {
        lines.close()
        process.stdin.destroy()
      }
    }, (error) => {
      response(request, "error", null, diagnostic(error))
    })
  } catch (error) {
    if (request) response(request, "error", null, diagnostic(error))
    else process.exitCode = 65
  }
})
