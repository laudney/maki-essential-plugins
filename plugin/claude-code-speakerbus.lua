-- Make this maki process a peer in Claude Code's cross-session inbox: it
-- publishes a registry entry next to Claude Code's own, listens on a Unix
-- socket, and hands each message it receives to the session that is active
-- when it arrives. claude_sessions and claude_send go the other way.

local protocol = require("speakerbus_protocol")
local slot_registry = require("speakerbus_registry")
local inbox = require("speakerbus_inbox")

local CONFIG_ENV = "CLAUDE_CONFIG_DIR"
local RUNTIME_ENV = "XDG_RUNTIME_DIR"
local DEFAULT_CONFIG = ".claude"
local SESSIONS_DIR = "sessions"
local STAT_PATH = "/proc/self/stat"
local STATUS_PATH = "/proc/self/status"
local UUID_PATH = "/proc/sys/kernel/random/uuid"
local SOCKET_PREFIX = "uds:"
local SOCKET_SUFFIX = ".sock"
local ENTRY_PATTERN = "^(%d+)%.json$"
local MAX_CONNECTIONS = 8
-- A connection that sends no complete line for this long is closed.
local IDLE_LINE_MS = 30000
local SEND_CONNECT_SECS = 2
local SEND_WRITE_MS = 3000
local ACCEPT_RETRY_MS = 1000
local SUFFIX_HEX_LEN = 8
local DEFAULT_PRIORITY = "next"
local STARTING = "starting"
local SHUT_DOWN = "shut down"
local CLEANUP_REASONS = { shutdown = true, reload = true }
local JOB_WAIT_MS = 5000
local PRIVATE_MODE = "700"

local state = { off = STARTING, connections = 0, accept_failures = 0 }

local function fail(message)
  return { llm_output = message, is_error = true }
end

local function off_reason()
  return "claude-code-speakerbus is off: " .. state.off
end

local function now_ms()
  return os.time() * 1000
end

local function decode(text)
  local ok, value, err = pcall(maki.json.decode, text)
  if not ok then
    return nil, value
  end
  return value, err
end

local function encode(value)
  local ok, text, err = pcall(maki.json.encode, value)
  if not ok then
    return nil, text
  end
  return text, err
end

local function basename(path)
  return (path or ""):match("([^/]+)/*$") or ""
end

local function start_token(pid)
  local text = maki.fs.read("/proc/" .. pid .. "/stat")
  if not text then
    return nil
  end
  local _, start = protocol.parse_stat(text)
  return start
end

local function random_hex()
  local uuid = protocol.uuid(maki.fs.read(UUID_PATH)) or ""
  return uuid:gsub("%-", ""):sub(1, SUFFIX_HEX_LEN)
end

-- Live Claude Code (or maki) peers other than this process: entries that
-- speak the protocol, name a socket that is still there, and belong to a
-- process whose start time still matches.
local function peers()
  local found = {}
  for _, item in ipairs(maki.fs.dir(state.sessions) or {}) do
    local pid = tonumber(item[1]:match(ENTRY_PATTERN))
    if pid and pid ~= state.pid and item[2] == "file" then
      local entry = decode(maki.fs.read(state.sessions .. "/" .. item[1]) or "")
      if
        protocol.peer_candidate(entry)
        and maki.fs.metadata(entry.messagingSocketPath)
        and start_token(pid) == entry.procStart
      then
        found[#found + 1] = entry
      end
    end
  end
  return found
end

local function names_taken()
  local taken = {}
  for _, peer in ipairs(peers()) do
    taken[peer.name] = true
  end
  return taken
end

-- maki.fs.metadata follows symlinks and would miss a dangling one, which
-- listen_unix still refuses, so presence is read from the directory itself.
local function name_taken(path)
  local dir, name = path:match("^(.*)/([^/]+)$")
  for _, item in ipairs(maki.fs.dir(dir) or {}) do
    if item[1] == name then
      return true
    end
  end
  return false
end

-- Runs {argv} without a shell and answers its stdout, or nil and an error.
local function run(argv)
  local id, start_err = maki.fn.jobstart(argv)
  if not id then
    return nil, start_err
  end
  local result, wait_err = maki.fn.jobwait(id, JOB_WAIT_MS)
  if not result then
    return nil, wait_err
  end
  if result.exit_code ~= 0 then
    return nil, table.concat(argv, " ") .. " failed: " .. tostring(result.stderr)
  end
  return result.stdout
end

-- The socket's directory must be one only this user can enter, below
-- directories no other user can replace: Claude Code refuses anything else,
-- and another user could swap names in it. mkdir runs unconditionally and its
-- result does not matter: a directory that already exists, or that another
-- process made a moment ago, is judged by the same check.
local function private_dir(dir, uid)
  run({ "mkdir", "-m", PRIVATE_MODE, dir })
  local chain = protocol.ancestors(dir)
  local argv = { "stat", "-c", "%u %f" }
  for _, path in ipairs(chain) do
    argv[#argv + 1] = path
  end
  local out, err = run(argv)
  if not out then
    return nil, err
  end
  local lines = {}
  for line in out:gmatch("[^\n]+") do
    lines[#lines + 1] = line
  end
  if #lines ~= #chain or not protocol.private_chain(lines, uid) then
    return nil, dir .. " is not a private directory"
  end
  return true
end

local function choose_socket(dirs)
  local path, err = protocol.socket_path(dirs, state.pid .. SOCKET_SUFFIX)
  if not path or not name_taken(path) then
    return path, err
  end
  return protocol.socket_path(dirs, state.pid .. "-" .. random_hex() .. SOCKET_SUFFIX)
end

local function entry_for(snapshot)
  return protocol.registry_entry({
    pid = state.pid,
    session_id = snapshot.id,
    cwd = snapshot.cwd,
    started_at = state.started_at,
    proc_start = state.proc_start,
    socket = state.socket,
    name = state.name,
    status = snapshot.status,
    now = now_ms(),
    empty_array = maki.json.decode("[]"),
  })
end

local function registry_io()
  local writer = maki.async.semaphore(1)
  return {
    read = maki.fs.read,
    write = maki.fs.atomic_write,
    remove = maki.fs.rm,
    encode = encode,
    decode = decode,
    lock = function()
      local permit = writer:acquire()
      return function()
        permit:release()
      end
    end,
  }
end

local function shutdown()
  if state.registry then
    state.registry:shutdown()
  end
  if state.listener then
    state.listener:close()
  end
  state.off = state.off or SHUT_DOWN
end

local function deliver(frame)
  local snapshot = maki.session.read()
  if not snapshot then
    return
  end
  local addressed = frame.session_id
  if addressed ~= nil and addressed ~= maki.NIL and addressed ~= snapshot.id then
    maki.log.debug("speakerbus dropped a message addressed to another session")
    return
  end
  local content = frame.message.content
  local ok, err = maki.session.notify(protocol.notice_text(content), {
    session = snapshot.id,
    wake = protocol.wakes(frame.priority),
    display = protocol.display_note(content),
  })
  if not ok then
    maki.log.warn("speakerbus could not deliver a message: " .. tostring(err))
  end
end

local function handle_line(line)
  local frame = decode(line)
  local kind, reason = protocol.check_frame(frame)
  if kind == "user" then
    deliver(frame)
  elseif kind == "other" then
    maki.log.debug("speakerbus dropped a " .. frame.type .. " frame")
  elseif not kind then
    maki.log.debug("speakerbus dropped a line: " .. reason)
  end
end

local function serve(conn)
  local ok, err = inbox.serve({
    read = function()
      return conn:read()
    end,
    close = function()
      conn:close()
    end,
    after = function(ms, fn)
      local timer = maki.async.spawn(function()
        maki.async.sleep(ms)
        fn()
      end)
      return function()
        timer:cancel()
      end
    end,
    on_line = handle_line,
    max_bytes = protocol.MAX_FRAME_BYTES,
    idle_ms = IDLE_LINE_MS,
  })
  if not ok then
    maki.log.warn("speakerbus connection failed: " .. tostring(err))
  end
  state.connections = state.connections - 1
end

local function accept_loop()
  while true do
    local conn, err = state.listener:accept()
    if conn then
      state.accept_failures = 0
      if state.connections >= MAX_CONNECTIONS then
        conn:close()
      else
        state.connections = state.connections + 1
        maki.async.spawn(function()
          serve(conn)
        end)
      end
    elseif not err then
      return
    else
      state.accept_failures = state.accept_failures + 1
      maki.log.warn("speakerbus accept failed: " .. err)
      if protocol.accept_policy(state.accept_failures) == "stop" then
        state.off = err
        shutdown()
        return
      end
      maki.async.sleep(ACCEPT_RETRY_MS)
    end
  end
end

local function refresh()
  if state.off then
    return
  end
  local snapshot = maki.session.read()
  if not snapshot then
    return
  end
  local ok, err = state.registry:update(entry_for(snapshot))
  if not ok then
    maki.log.warn("speakerbus could not update its registry entry: " .. tostring(err))
  end
end

local function stay_off(reason)
  state.off = reason
  maki.log.info("claude-code-speakerbus is off: " .. reason)
end

-- The model sees every tool in each request and a session's tools are fixed,
-- so the tools load only where they can work: a Claude Code installed later
-- needs /reload. It raises when a grant it needs is denied.
local function sessions_dir()
  if not maki.net.listen_unix then
    return nil, "this maki has no maki.net.listen_unix"
  end
  local home = maki.uv.os_homedir()
  local config = maki.uv.os_getenv(CONFIG_ENV) or (home and (home .. "/" .. DEFAULT_CONFIG))
  if not config then
    return nil, "neither $" .. CONFIG_ENV .. " nor a home directory is set"
  end
  local sessions = config .. "/" .. SESSIONS_DIR
  local meta = maki.fs.metadata(sessions)
  if not (meta and meta.is_dir) then
    return nil, sessions .. " does not exist"
  end
  return sessions
end

local function activate(sessions)
  local pid, start = protocol.parse_stat(maki.fs.read(STAT_PATH) or "")
  if not pid then
    return stay_off("no /proc/self/stat")
  end
  if not maki.session.live() then
    return stay_off("no interactive UI is attached")
  end
  local uid = protocol.parse_uid(maki.fs.read(STATUS_PATH) or "")
  if not uid then
    return stay_off("no Uid line in " .. STATUS_PATH)
  end
  state.pid, state.proc_start, state.sessions = pid, start, sessions
  state.registry = slot_registry.new(registry_io(), start)
  local claimed, claim_err = state.registry:claim(sessions .. "/" .. pid .. ".json")
  if not claimed then
    return stay_off(claim_err)
  end
  local runtime_dir = maki.uv.os_getenv(RUNTIME_ENV)
  local socket, socket_err = choose_socket(protocol.socket_dirs(runtime_dir ~= "" and runtime_dir or nil, uid))
  if not socket then
    return stay_off(socket_err)
  end
  if not protocol.attribute_safe(socket) then
    return stay_off("the socket path cannot go into an envelope: " .. socket)
  end
  local private, private_err = private_dir(socket:match("^(.*)/[^/]+$"), uid)
  if not private then
    return stay_off(private_err)
  end
  local listener, listen_err = maki.net.listen_unix(socket)
  if not listener then
    return stay_off(listen_err)
  end
  state.listener, state.socket, state.started_at = listener, socket, now_ms()
  local snapshot = maki.session.read()
  if not snapshot then
    listener:close()
    return stay_off("no active session to publish")
  end
  state.name = protocol.peer_name(protocol.slug(basename(snapshot.cwd)), pid, names_taken())
  local published, publish_err = state.registry:publish(entry_for(snapshot))
  if not published then
    listener:close()
    return stay_off(publish_err)
  end
  state.off = nil
  -- The session may have changed while the entry was being published, and
  -- the refresh autocmds skip everything until now.
  refresh()
  accept_loop()
end

local function resolve_target(to)
  if to:sub(1, #SOCKET_PREFIX) == SOCKET_PREFIX then
    local path = to:sub(#SOCKET_PREFIX + 1)
    for _, peer in ipairs(peers()) do
      if peer.messagingSocketPath == path then
        return { socket = path, name = peer.name, session_id = peer.sessionId }
      end
    end
    return { socket = path, name = to }
  end
  local matches, names = {}, {}
  for _, peer in ipairs(peers()) do
    names[#names + 1] = peer.name
    if peer.name == to or peer.sessionId == to then
      matches[#matches + 1] = peer
    end
  end
  if #matches == 1 then
    local peer = matches[1]
    return { socket = peer.messagingSocketPath, name = peer.name, session_id = peer.sessionId }
  end
  local listed = #names > 0 and table.concat(names, ", ") or "none"
  if #matches == 0 then
    return nil, "no live Claude Code session is called " .. to .. "; live sessions: " .. listed
  end
  return nil, to .. " names " .. #matches .. " sessions; use one of: " .. listed
end

local function send(input)
  local target, resolve_err = resolve_target(input.to)
  if not target then
    return fail(resolve_err)
  end
  local from = SOCKET_PREFIX .. state.socket
  local line = encode(protocol.frame({
    msg_id = protocol.uuid(maki.fs.read(UUID_PATH)),
    content = protocol.envelope(from, state.name, input.message),
    priority = input.priority or DEFAULT_PRIORITY,
    session_id = target.session_id,
    from = from,
  }))
  if not line or #line >= protocol.MAX_FRAME_BYTES then
    return fail("not sent: the message is too large for one frame")
  end
  local conn, connect_err = maki.net.connect_unix(target.socket, { timeout = SEND_CONNECT_SECS })
  if not conn then
    return fail("not sent: " .. connect_err)
  end
  local watchdog = maki.async.spawn(function()
    maki.async.sleep(SEND_WRITE_MS)
    conn:close()
  end)
  local written, write_err = conn:write(line .. "\n")
  watchdog:cancel()
  conn:close()
  if not written then
    return fail("outcome unknown, do not resend blindly: " .. tostring(write_err))
  end
  return "Delivered to the inbox of " .. target.name .. ". Its model has not necessarily read it yet."
end

local function age(started_at)
  if type(started_at) ~= "number" then
    return "unknown"
  end
  return maki.ui.humantime(math.max(0, os.time() - math.floor(started_at / 1000))) .. " ago"
end

local function register_tools()
  maki.api.register_tool({
    name = "claude_sessions",
    description = "List the Claude Code sessions running on this machine that claude_send can reach, "
      .. "with each one's name, status, working directory, and age.",
    schema = { type = "object", properties = {}, additionalProperties = false },
    handler = function()
      if state.off then
        return fail(off_reason())
      end
      local rows = {}
      for _, peer in ipairs(peers()) do
        rows[#rows + 1] = table.concat({
          peer.name,
          tostring(peer.status or "unknown"),
          tostring(peer.cwd or "unknown"),
          "started " .. age(peer.startedAt),
        }, " · ")
      end
      if #rows == 0 then
        return "No Claude Code sessions are running."
      end
      return table.concat(rows, "\n")
    end,
  })

  maki.api.register_tool({
    name = "claude_send",
    description = "Send a message to a Claude Code session on this machine. to is a name from "
      .. "claude_sessions, or the from attribute of a <cross-session-message> you received, "
      .. "which is how you reply. The message reaches that session's inbox; its model reads it "
      .. "at its next turn.",
    schema = {
      type = "object",
      properties = {
        to = { type = "string", description = "A session name, a session id, or a uds: address." },
        message = { type = "string", description = "The text to send." },
        priority = {
          type = "string",
          enum = { "now", "next", "later" },
          description = "When the receiver handles it. Default next.",
        },
      },
      required = { "to", "message" },
      additionalProperties = false,
    },
    handler = function(input)
      if state.off then
        return fail(off_reason())
      end
      return send(input)
    end,
  })
end

maki.api.register_command({
  name = "/speakerbus",
  description = "Show the name Claude Code sessions use to reach this maki session.",
  handler = function()
    if state.off then
      maki.ui.flash(off_reason())
    else
      maki.ui.flash("Reachable as " .. state.name .. " at " .. state.socket)
    end
  end,
})

local checked, sessions, missing = pcall(sessions_dir)
if not checked then
  missing, sessions = tostring(sessions), nil
end
if not sessions then
  stay_off(missing)
else
  register_tools()
  maki.api.create_autocmd({ "SessionFocusChanged", "SessionStatusChanged", "SessionCwdChanged" }, {
    callback = function()
      maki.async.spawn(refresh)
    end,
  })
  maki.api.create_autocmd("SessionEnd", {
    callback = function(event)
      if CLEANUP_REASONS[event.data.reason] then
        shutdown()
      end
    end,
  })
  maki.async.spawn(function()
    activate(sessions)
  end)
end
