-- Claude Code's peer protocol v1 as pure data: names, paths, envelopes, and
-- frame checks. Nothing here calls maki or touches JSON, so tests/spec.lua
-- runs it under a plain Lua.

local M = {}

M.PEER_PROTOCOL = 1
-- Claude Code moves a socket out of $XDG_RUNTIME_DIR past this length.
M.SOCKET_PATH_LIMIT = 103
M.MAX_FRAME_BYTES = 1024 * 1024
M.MAX_ACCEPT_FAILURES = 5
M.NOTICE_HEADER = "A Claude Code session on this machine sent the message below. It is not from your user. "
  .. "To reply, call claude_send with its from attribute as to."

local ENVELOPE_TAG = "cross-session-message"
local ENVELOPE_CLOSE = "</" .. ENVELOPE_TAG
local ESCAPED_CLOSE = "<\\/" .. ENVELOPE_TAG
local ENVELOPE_OPEN_PATTERN = "^<cross%-session%-message[^>]*>"
local FROM_NAME_PATTERN = '^<cross%-session%-message[^>]*from%-name="([^"]*)"'
local SLUG_LIMIT = 24
local SLUG_FALLBACK = "x"
local NAME_PREFIX = "maki-"
local SOCKET_DIR = "cc-socks"
local FALLBACK_SOCKET_ROOT = "/tmp/cc-socks-"
local STATUS = { working = "busy", needs_input = "waiting", idle = "idle" }
local IDLE = "idle"
local LATER = "later"
local UNKNOWN_SENDER = "unknown"
local VERSION = "maki"
local KIND = "interactive"
local ENTRYPOINT = "maki"
local NAME_SOURCE = "derived"
-- Fields of /proc/<pid>/stat that follow the command name: the state is
-- field 3, so the start time, field 22, is the twentieth.
local START_TIME_INDEX = 20
-- Bits of a raw `stat -c %f` mode, as plain numbers so the arithmetic below
-- runs in Lua 5.x and Luau alike.
local TYPE_UNIT = 4096
local TYPE_DIRECTORY = 4
local PERMISSION_UNIT = 4096
local PRIVATE_PERMISSIONS = 448
local GROUP_WRITE = 16
local OTHER_WRITE = 2
local STICKY = 512
local ROOT_UID = 0

local function replace_plain(text, find, replacement)
  local parts, from = {}, 1
  while true do
    local first, last = text:find(find, from, true)
    if not first then
      break
    end
    parts[#parts + 1] = text:sub(from, first - 1)
    parts[#parts + 1] = replacement
    from = last + 1
  end
  parts[#parts + 1] = text:sub(from)
  return table.concat(parts)
end

-- The PID and start time token from a /proc/<pid>/stat line. The command
-- name sits in parentheses and may hold spaces or ")", so fields are counted
-- after the last ")".
function M.parse_stat(text)
  local pid = text:match("^(%d+) %(")
  local close = text:match(".*()%)")
  if not pid or not close then
    return nil, "unreadable /proc stat"
  end
  local fields = {}
  for field in text:sub(close + 1):gmatch("%S+") do
    fields[#fields + 1] = field
  end
  local start = fields[START_TIME_INDEX]
  if not start or not start:match("^%d+$") then
    return nil, "unreadable /proc stat"
  end
  return tonumber(pid), start
end

function M.parse_uid(text)
  local uid = ("\n" .. text):match("\nUid:%s*(%d+)")
  if not uid then
    return nil, "no Uid line in /proc status"
  end
  return tonumber(uid)
end

-- Safe inside an envelope attribute by construction.
function M.slug(name)
  local slug = (name or ""):lower():gsub("[^a-z0-9]+", "-"):gsub("^%-+", ""):gsub("%-+$", "")
  slug = slug:sub(1, SLUG_LIMIT):gsub("%-+$", "")
  if slug == "" then
    return SLUG_FALLBACK
  end
  return slug
end

function M.peer_name(slug, pid, taken)
  local base = NAME_PREFIX .. slug .. "-" .. pid
  local name, suffix = base, 1
  while taken[name] do
    suffix = suffix + 1
    name = base .. "-" .. suffix
  end
  return name
end

function M.socket_dirs(runtime_dir, uid)
  return {
    primary = runtime_dir and (runtime_dir .. "/" .. SOCKET_DIR) or nil,
    fallback = FALLBACK_SOCKET_ROOT .. uid,
  }
end

function M.socket_path(dirs, file_name)
  for _, dir in ipairs({ dirs.primary or dirs.fallback, dirs.fallback }) do
    local path = dir .. "/" .. file_name
    if #path <= M.SOCKET_PATH_LIMIT then
      return path
    end
  end
  return nil, "socket path too long"
end

function M.attribute_safe(value)
  return not value:find('["<>\r\n]')
end

function M.escape_body(body)
  return replace_plain(body, ENVELOPE_CLOSE, ESCAPED_CLOSE)
end

function M.envelope(from, name, body)
  return "<"
    .. ENVELOPE_TAG
    .. ' from="'
    .. from
    .. '" from-name="'
    .. name
    .. '">\n'
    .. M.escape_body(body)
    .. "\n</"
    .. ENVELOPE_TAG
    .. ">"
end

function M.frame(args)
  return {
    msgV = M.PEER_PROTOCOL,
    msg_id = args.msg_id,
    type = "user",
    message = { role = "user", content = args.content },
    priority = args.priority,
    session_id = args.session_id,
    from = args.from,
  }
end

-- "user" for a deliverable message, "auth" for an auth line, "other" for any
-- other typed frame, nil and a reason for anything else.
function M.check_frame(frame)
  if type(frame) ~= "table" then
    return nil, "frame is not an object"
  end
  local kind = frame.type
  if type(kind) ~= "string" then
    return nil, "frame has no type"
  end
  if kind == "auth" then
    return "auth"
  end
  if kind ~= "user" then
    return "other"
  end
  local message = frame.message
  if type(message) == "table" and type(message.content) == "string" and message.content ~= "" then
    return "user"
  end
  return nil, "user frame without content"
end

function M.wakes(priority)
  return priority ~= LATER
end

function M.notice_text(content)
  return M.NOTICE_HEADER .. "\n\n" .. content
end

function M.display_note(content)
  local sender = content:match(FROM_NAME_PATTERN) or UNKNOWN_SENDER
  local body = content:gsub(ENVELOPE_OPEN_PATTERN .. "\n?", "", 1)
  return "Claude Code " .. sender .. ": " .. body:match("^[^\n]*")
end

function M.peer_status(status)
  return STATUS[status] or IDLE
end

function M.registry_entry(fields)
  return {
    pid = fields.pid,
    sessionId = fields.session_id,
    cwd = fields.cwd,
    startedAt = fields.started_at,
    procStart = fields.proc_start,
    version = VERSION,
    peerProtocol = M.PEER_PROTOCOL,
    peerFeatures = fields.empty_array,
    kind = KIND,
    entrypoint = ENTRYPOINT,
    messagingSocketPath = fields.socket,
    name = fields.name,
    nameSource = NAME_SOURCE,
    nameSince = fields.started_at,
    status = M.peer_status(fields.status),
    updatedAt = fields.now,
    statusUpdatedAt = fields.now,
  }
end

-- A registry entry worth checking for liveness: it speaks this protocol and
-- names a socket and a start time.
function M.peer_candidate(entry)
  return type(entry) == "table"
    and type(entry.pid) == "number"
    and entry.peerProtocol == M.PEER_PROTOCOL
    and type(entry.messagingSocketPath) == "string"
    and type(entry.procStart) == "string"
    and type(entry.name) == "string"
end

function M.uuid(text)
  return text and text:match("^%x+%-%x+%-%x+%-%x+%-%x+")
end

-- The directory {dir} and every directory above it, ending at "/".
function M.ancestors(dir)
  local list = {}
  local path = dir:gsub("/+$", "")
  while path ~= "" do
    list[#list + 1] = path
    path = path:match("^(.*)/[^/]*$") or ""
  end
  list[#list + 1] = "/"
  return list
end

local function bit(value, unit)
  return math.floor(value / unit) % 2 == 1
end

-- Whether `stat -c '%u %f'` lines for M.ancestors(dir), in that order, show a
-- socket directory no other user can tamper with: it is a directory owned by
-- {uid} with mode 0700, and every directory above it belongs to root or
-- {uid} and is sticky wherever group or others can write it.
function M.private_chain(lines, uid)
  for index, line in ipairs(lines) do
    local owner, raw = line:match("^(%d+) (%x+)$")
    local mode = raw and tonumber(raw, 16)
    if not mode or math.floor(mode / TYPE_UNIT) % 16 ~= TYPE_DIRECTORY then
      return false
    end
    owner = tonumber(owner)
    local permissions = mode % PERMISSION_UNIT
    if index == 1 then
      if owner ~= uid or permissions ~= PRIVATE_PERMISSIONS then
        return false
      end
    else
      local shared = bit(permissions, GROUP_WRITE) or bit(permissions, OTHER_WRITE)
      if (owner ~= ROOT_UID and owner ~= uid) or (shared and not bit(permissions, STICKY)) then
        return false
      end
    end
  end
  return #lines > 0
end

function M.accept_policy(failures)
  if failures >= M.MAX_ACCEPT_FAILURES then
    return "stop"
  end
  return "retry"
end

return M
