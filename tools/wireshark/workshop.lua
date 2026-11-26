--[[
  Wireshark dissectors for the esp32-rust-workshop network protocols.

  Three protocols, three Proto objects:

    wsnode  UDP/TCP 7000  the node frames ($DAT,node-A,7,12500,1,2,2500000*08) and the address space
                          frames (UA-lite: BRW BRR RED WRT SUB VAL). One ASCII line per UDP datagram,
                          lines in a TCP stream. Checksum, sequence gaps, request/answer links and
                          round-trip times (ECH) are computed.
    wsgen   UDP 7001      the traffic generator's datagrams (`gen` in the CLI): "WGEN" header, loss,
                          order and RFC 3550 jitter per stream.
    wsadc   UDP 7100      the ADC streaming firmware: ADC1 sample datagrams, LOG1/RPL1 log datagrams, POT1
                          (the potentiometer's live position), and the ASCII commands (`start rate=20000
                          n=256`, `stop`, `info`, `pot freq`...).

  Install:  ln -s "$PWD/tools/wireshark/workshop.lua" ~/.local/lib/wireshark/plugins/workshop.lua
  (the folder is the "Personal Lua Plugins" one of Help > About Wireshark > Folders), then restart
  Wireshark, or reload with Ctrl+Shift+L. Ports are in Edit > Preferences > Protocols (net.port is
  configurable on the boards, 1024-65534).

  The tables of the address space and of the settings mirror firmware/shared/core/src/space.rs and
  config.rs; tools/wireshark/check.py fails when they drift apart.
]]

local VERSION = "1.0"

set_plugin_info({
  version = VERSION,
  author = "esp32-rust-workshop",
  description = "Dissectors for the workshop node frames, the traffic generator and the ADC stream",
})

-- ------------------------------------------------------------------------------------------------
-- Helpers
-- ------------------------------------------------------------------------------------------------

local bxor
if bit32 then
  bxor = bit32.bxor
elseif bit then
  bxor = bit.bxor
else
  bxor = load("return function(a, b) return a ~ b end")()
end

local TWO32 = 4294967296

--- Mirror of `workshop_core::seq::check`: what a sequence number says compared with the previous one.
local function seq_check(prev, seq)
  if prev == nil then return "first", 0 end
  local d = (seq - prev) % TWO32
  if d == 1 then return "ok", 0 end
  if d >= 2 and d <= 2147483647 then return "lost", d - 1 end
  return "restart", 0
end

local function plural(n, word)
  return string.format("%d %s%s", n, word, n == 1 and "" or "s")
end

local udp_table = DissectorTable.get("udp.port")
local tcp_table = DissectorTable.get("tcp.port")
local f_tcp_srcport = Field.new("tcp.srcport")

--- Keeps a protocol on the ports its preferences name (a port is also removed when the preference changes).
local function follow_ports(proto, tables, current)
  return function()
    for _, spec in ipairs(current) do
      local wanted = proto.prefs[spec.pref]
      if spec.port ~= wanted then
        for _, tbl in ipairs(tables) do
          if spec.port then tbl:remove(spec.port, proto) end
          tbl:add(wanted, proto)
        end
        spec.port = wanted
      end
    end
  end
end

-- ------------------------------------------------------------------------------------------------
-- The address space (UA-lite): ids, paths, status codes
-- ------------------------------------------------------------------------------------------------

local NONE_ID = 65535
local CONFIG_BASE = 100

-- NODES of space.rs: id, parent, name.
local NODES = {
  { id = 0, parent = 65535, name = "" },
  { id = 1, parent = 0, name = "node" },
  { id = 2, parent = 1, name = "name" },
  { id = 3, parent = 1, name = "uptime" },
  { id = 4, parent = 0, name = "leds" },
  { id = 5, parent = 4, name = "1" },
  { id = 6, parent = 4, name = "2" },
  { id = 7, parent = 0, name = "buttons" },
  { id = 8, parent = 7, name = "jumper" },
  { id = 9, parent = 7, name = "boot" },
  { id = 10, parent = 0, name = "rgb" },
  { id = 11, parent = 10, name = "pattern" },
  { id = 12, parent = 10, name = "brightness" },
  { id = 13, parent = 0, name = "stats" },
  { id = 14, parent = 13, name = "tx" },
  { id = 15, parent = 13, name = "rx" },
  { id = 16, parent = 13, name = "lost" },
  { id = 17, parent = 0, name = "config" },
}

-- KEYS of config.rs, in order: /config/<name> has the id CONFIG_BASE + index.
local CONFIG_KEYS = {
  "data_period_ms",
  "status_period_ms",
  "link_timeout",
  "debounce_ms",
  "lockout_ms",
  "rgb.pattern",
  "rgb.freq_hz",
  "rgb.cycle_ms",
  "rgb.brightness",
  "led1",
  "led2",
  "verbose",
  "tx_mode",
  "burst.count",
  "burst.rate_hz",
  "console",
  "trace",
  "trace_bytes",
  "trace_view",
  "trace_dash",
  "trace_dash_view",
  "trace_window_s",
  "trace_short_ip",
}

-- Status of space.rs, by wire code.
local STATUS = {
  [0] = "Good",
  [1] = "BadNodeIdUnknown",
  [2] = "BadNotWritable",
  [3] = "BadTypeMismatch",
  [4] = "BadOutOfRange",
  [5] = "BadTooManySubscriptions",
}

local NODE_BY_ID = {}
for _, n in ipairs(NODES) do NODE_BY_ID[n.id] = n end

--- `/leds/1` for 5; `/` for the root; `/config/led1` for 100 + 9; "?" when the id is unknown.
local function node_path(id)
  if id == NONE_ID then return "(none / all)" end
  if id >= CONFIG_BASE then
    local key = CONFIG_KEYS[id - CONFIG_BASE + 1]
    return key and ("/config/" .. key) or "?"
  end
  local parts = {}
  local n = NODE_BY_ID[id]
  if not n then return "?" end
  while n and n.parent ~= NONE_ID do
    table.insert(parts, 1, n.name)
    n = NODE_BY_ID[n.parent]
  end
  return "/" .. table.concat(parts, "/")
end

local KIND_CLASS = { O = "Object", V = "Variable" }
local KIND_TYPE = { B = "bool", U = "u32", T = "text", ["-"] = "no value" }
local KIND_ACCESS = { R = "read only", W = "read-write" }

local function describe_kind(kind)
  if kind == "---" then return "no node (end of the branch)" end
  local c, t, a = kind:sub(1, 1), kind:sub(2, 2), kind:sub(3, 3)
  if not (KIND_CLASS[c] and KIND_TYPE[t] and KIND_ACCESS[a]) then return nil end
  return string.format("%s, %s, %s", KIND_CLASS[c], KIND_TYPE[t], KIND_ACCESS[a])
end

-- ------------------------------------------------------------------------------------------------
-- wsnode: the node and address space frames
-- ------------------------------------------------------------------------------------------------

local node = Proto("wsnode", "Workshop node frame")

node.prefs.port = Pref.uint("Port (UDP and TCP)", 7000, "net.port of the boards (1024-65534)")

local ND = {
  tag = ProtoField.string("wsnode.tag", "Type"),
  node = ProtoField.string("wsnode.node", "Node"),
  seq = ProtoField.uint32("wsnode.seq", "Sequence number"),
  burst = ProtoField.uint32("wsnode.burst", "Burst number"),
  t_ms = ProtoField.uint32("wsnode.t_ms", "Sender uptime (ms)"),
  checksum = ProtoField.string("wsnode.checksum", "Checksum"),
  cs_ok = ProtoField.bool("wsnode.checksum.good", "Checksum is good"),
  cs_calc = ProtoField.string("wsnode.checksum.calculated", "Calculated checksum"),
  -- DAT
  btn = ProtoField.bool("wsnode.button", "Button pressed"),
  events = ProtoField.uint32("wsnode.events", "EVT frames sent since boot"),
  period_us = ProtoField.uint32("wsnode.period_us", "Sender's DAT period (µs)"),
  -- EVT
  edge = ProtoField.string("wsnode.edge", "Edge"),
  key = ProtoField.string("wsnode.key", "TRNG key"),
  -- ECH
  echo_seq = ProtoField.uint32("wsnode.echo_seq", "Echo of sequence number"),
  rtt = ProtoField.double("wsnode.rtt", "Round-trip time (ms)"),
  -- BAU
  baud = ProtoField.uint32("wsnode.baud", "Baud rate"),
  ack = ProtoField.string("wsnode.baud_phase", "Phase"),
  -- BST
  index = ProtoField.uint32("wsnode.index", "Index"),
  count = ProtoField.uint32("wsnode.count", "Burst length"),
  fill = ProtoField.string("wsnode.fill", "Fill"),
  -- space
  id = ProtoField.uint16("wsnode.id", "Node id"),
  path = ProtoField.string("wsnode.path", "Path"),
  req = ProtoField.uint32("wsnode.req", "Answers request (seq)"),
  child_id = ProtoField.uint16("wsnode.child_id", "Child id"),
  child_path = ProtoField.string("wsnode.child_path", "Child path"),
  name = ProtoField.string("wsnode.name", "Name"),
  kind = ProtoField.string("wsnode.kind", "Kind"),
  kind_text = ProtoField.string("wsnode.kind.text", "Kind, spelled out"),
  more = ProtoField.bool("wsnode.more", "More children"),
  value = ProtoField.string("wsnode.value", "Value"),
  interval = ProtoField.uint32("wsnode.interval_ms", "Interval (ms)"),
  status = ProtoField.uint8("wsnode.status", "Status", base.DEC, STATUS),
  request_in = ProtoField.framenum("wsnode.request_in", "Request in", base.NONE, frametype.REQUEST),
  response_in = ProtoField.framenum("wsnode.response_in", "Answer in", base.NONE, frametype.RESPONSE),
  echo_of = ProtoField.framenum("wsnode.echo_of", "Echo of frame", base.NONE, frametype.REQUEST),
  lost = ProtoField.uint32("wsnode.lost", "Frames lost before this one"),
}

local ndf = {}
for _, f in pairs(ND) do ndf[#ndf + 1] = f end
node.fields = ndf

local ex_bad_cs = ProtoExpert.new("wsnode.bad_checksum", "Checksum does not match",
  expert.group.CHECKSUM, expert.severity.WARN)
local ex_malformed = ProtoExpert.new("wsnode.malformed", "Malformed frame",
  expert.group.MALFORMED, expert.severity.ERROR)
local ex_lost = ProtoExpert.new("wsnode.lost_frames", "Frames lost: a gap in the sender's sequence numbers",
  expert.group.SEQUENCE, expert.severity.WARN)
local ex_restart = ProtoExpert.new("wsnode.restart", "Sequence number went back: the sender restarted (or frames were reordered)",
  expert.group.SEQUENCE, expert.severity.NOTE)
local ex_status = ProtoExpert.new("wsnode.bad_status", "The node refused the request",
  expert.group.RESPONSE_CODE, expert.severity.WARN)
local ex_long = ProtoExpert.new("wsnode.too_long", "Longer than 96 bytes: a node would refuse this frame",
  expert.group.MALFORMED, expert.severity.WARN)
node.experts = { ex_bad_cs, ex_malformed, ex_lost, ex_restart, ex_status, ex_long }

-- Per capture, reset by node.init.
local n_last_seq   -- "ip:node" -> last seq
local n_seq_result -- "frame:line" -> { kind, lost }
local n_seen       -- "src>dst:seq" -> { frame, time }
local n_answer     -- "src>dst:seq" -> { frame (of the request), answer = frame of the first answer }

local function reset_node() n_last_seq, n_seq_result, n_seen, n_answer = {}, {}, {}, {} end
node.init = reset_node
reset_node()

-- Fields after the common four (tag, node, seq, t_ms), by frame type: name, how it is read.
local SCHEMA = {
  DAT = { { "btn", "bool" }, { "events", "u32" }, { "period_us", "u32" } },
  EVT = { { "edge", "edge" }, { "key", "hex" } },
  ECH = { { "echo_seq", "u32" } },
  BAU = { { "baud", "u32" }, { "ack", "phase" } },
  BST = { { "index", "u32" }, { "count", "u32" }, { "fill", "hex" } },
  BRW = { { "id", "u16" }, { "index", "u32" } },
  BRR = { { "req", "u32" }, { "child_id", "u16" }, { "name", "str" }, { "kind", "kind" }, { "more", "bool" } },
  RED = { { "id", "u16" } },
  WRT = { { "id", "u16" }, { "value", "str" } },
  SUB = { { "id", "u16" }, { "interval", "u32" } },
  VAL = { { "req", "u32" }, { "id", "u16" }, { "status", "u8" }, { "value", "str" } },
}

local TYPE_NAMES = {
  DAT = "periodic data",
  EVT = "button event",
  ECH = "echo",
  BAU = "baud rate change",
  BST = "burst",
  BRW = "browse",
  BRR = "browse result",
  RED = "read",
  WRT = "write",
  SUB = "subscribe",
  VAL = "value",
}

local function split_fields(body)
  local out, pos = {}, 1
  while true do
    local comma = body:find(",", pos, true)
    if not comma then
      out[#out + 1] = { t = body:sub(pos), off = pos - 1 }
      return out
    end
    out[#out + 1] = { t = body:sub(pos, comma - 1), off = pos - 1 }
    pos = comma + 1
  end
end

local function xor_checksum(body)
  local cs = 0
  for i = 1, #body do cs = bxor(cs, body:byte(i)) end
  return cs
end

local LIMITS = { u8 = 255, u16 = 65535, u32 = 4294967295 }

--- Dissects one line (`len` bytes at `base` of `tvb`, without its newline). `line_no` numbers the lines of
--- a packet (TCP can carry several). Returns the line's summary for the Info column.
local function dissect_line(tvb, pinfo, tree, base, len, line_no, wire_len)
  local s = tvb:raw(base, len)
  local item = tree:add(node, tvb(base, len))

  local body, cs = s:match("^%$(.-)%*(%x%x)$")
  if not body then
    item:append_text(": not a frame")
    item:add_proto_expert_info(ex_malformed, "A frame is $TYPE,node,seq,t_ms,...*CS")
    return "(not a frame)"
  end

  local f = split_fields(body)
  local tag = f[1].t
  local schema = SCHEMA[tag]
  local function range(i) return tvb(base + 1 + f[i].off, #f[i].t) end

  -- Common head.
  local sub = item
  item:add(ND.tag, range(1)):append_text(TYPE_NAMES[tag] and (" (" .. TYPE_NAMES[tag] .. ")") or "")
  if #f < 4 then
    item:append_text(": " .. tag .. " (too short)")
    item:add_proto_expert_info(ex_malformed, "Fewer than 4 fields")
    return tag .. " (too short)"
  end
  item:add(ND.node, range(2))
  local node_name = f[2].t
  local seq = f[3].t:match("^%d+$") and tonumber(f[3].t)
  local t_ms = f[4].t:match("^%d+$") and tonumber(f[4].t)
  local is_burst = tag == "BST"
  if seq and seq <= LIMITS.u32 then
    item:add(is_burst and ND.burst or ND.seq, range(3), seq)
  else
    item:add_proto_expert_info(ex_malformed, "sequence number is not a number")
    seq = nil
  end
  if t_ms and t_ms <= LIMITS.u32 then item:add(ND.t_ms, range(4), t_ms) end

  -- Type specific fields.
  local v = {}
  local problems = {}
  if not schema then
    table.insert(problems, "unknown frame type " .. tag)
  else
    if #f ~= 4 + #schema then
      table.insert(problems, string.format("%s has %d fields, %d expected", tag, #f, 4 + #schema))
    end
    for i, spec in ipairs(schema) do
      local name, kind = spec[1], spec[2]
      local fi = f[4 + i]
      if fi then
        local text = fi.t
        local r = range(4 + i)
        if kind == "u8" or kind == "u16" or kind == "u32" then
          local n = text:match("^%d+$") and tonumber(text)
          if n and n <= LIMITS[kind] then
            v[name] = n
            item:add(ND[name], r, n)
          else
            table.insert(problems, name .. " is not a number")
          end
        elseif kind == "bool" then
          if text == "0" or text == "1" then
            v[name] = text == "1"
            item:add(ND[name], r, text == "1")
          else
            table.insert(problems, name .. " is not 0 or 1")
          end
        elseif kind == "edge" then
          v[name] = text
          item:add(ND[name], r):append_text(text == "RISE" and " (jumper released)"
            or text == "FALL" and " (jumper plugged to GND: pressed)" or "")
          if text ~= "RISE" and text ~= "FALL" then table.insert(problems, "edge is not RISE or FALL") end
        elseif kind == "phase" then
          v[name] = text
          item:add(ND[name], r)
          if text ~= "REQ" and text ~= "ACK" then table.insert(problems, "phase is not REQ or ACK") end
        elseif kind == "kind" then
          v[name] = text
          local k = item:add(ND.kind, r)
          local d = describe_kind(text)
          if d then k:add(ND.kind_text, r, d):set_generated() else table.insert(problems, "kind is not 3 letters like VTW") end
        else
          v[name] = text
          item:add(ND[name], r)
        end
      end
    end
  end

  -- Path of the node an id names.
  if v.id then item:add(ND.path, range(4 + 1 + (tag == "VAL" and 1 or 0)), node_path(v.id)):set_generated() end
  if v.child_id then
    item:add(ND.child_path, range(4 + 2), node_path(v.child_id)):set_generated()
  end

  -- Checksum.
  local calc = xor_checksum(body)
  local calc_text = string.format("%02X", calc)
  local cs_item = item:add(ND.checksum, tvb(base + len - 2, 2))
  local good = cs == calc_text
  cs_item:add(ND.cs_ok, good):set_generated()
  if not good then
    cs_item:add(ND.cs_calc, calc_text):set_generated()
    cs_item:add_proto_expert_info(ex_bad_cs, "expected " .. calc_text .. ", the frame says " .. cs)
  end
  if wire_len > 96 then item:add_proto_expert_info(ex_long) end

  for _, p in ipairs(problems) do item:add_proto_expert_info(ex_malformed, p) end

  -- Sequence check, as the nodes do it: the sequence counter of a sender is shared by every frame but BST.
  local who = tostring(pinfo.src) .. ":" .. node_name
  local result_key = pinfo.number .. ":" .. line_no
  if seq and not is_burst and good then
    local res = n_seq_result[result_key]
    if not res then
      local kind, lost = seq_check(n_last_seq[who], seq)
      res = { kind = kind, lost = lost }
      n_seq_result[result_key] = res
      n_last_seq[who] = seq
    end
    if res.kind == "lost" then
      item:add(ND.lost, res.lost):set_generated()
      item:add_proto_expert_info(ex_lost, plural(res.lost, "frame") .. " lost before this one")
    elseif res.kind == "restart" then
      item:add_proto_expert_info(ex_restart)
    end
  end

  -- Request / answer links, and the round-trip time of an echo.
  local ts = pinfo.abs_ts
  local from_to = tostring(pinfo.src) .. ">" .. tostring(pinfo.dst)
  local back = tostring(pinfo.dst) .. ">" .. tostring(pinfo.src)
  if seq and not is_burst and good then
    n_seen[from_to .. ":" .. seq] = { frame = pinfo.number, time = ts }
  end
  local answered_seq = (tag == "ECH" and v.echo_seq) or ((tag == "BRR" or tag == "VAL") and v.req) or nil
  local answered
  if answered_seq then
    answered = n_seen[back .. ":" .. answered_seq]
    if answered and answered.frame < pinfo.number or (answered and answered.frame == pinfo.number) then
      local key = back .. ":" .. answered_seq
      local a = n_answer[key]
      if not a or a.request ~= answered.frame then
        n_answer[key] = { request = answered.frame, answer = pinfo.number }
      end
      if tag == "ECH" then
        item:add(ND.echo_of, answered.frame):set_generated()
        item:add(ND.rtt, (ts - answered.time) * 1000):set_generated()
      else
        item:add(ND.request_in, answered.frame):set_generated()
      end
    else
      answered = nil
    end
  end
  if seq and not is_burst and (tag == "BRW" or tag == "RED" or tag == "WRT" or tag == "SUB") then
    local a = n_answer[from_to .. ":" .. seq]
    if a and a.request == pinfo.number then item:add(ND.response_in, a.answer):set_generated() end
  end
  if tag == "VAL" and v.status and v.status ~= 0 then
    item:add_proto_expert_info(ex_status, STATUS[v.status] or ("status " .. v.status))
  end

  -- The Info column.
  local info
  local who_seq = string.format("%s seq=%s", node_name, seq and tostring(seq) or "?")
  if tag == "DAT" then
    info = string.format("DAT %s button=%s events=%s period=%s µs", who_seq, v.btn == nil and "?" or (v.btn and "1" or "0"),
      tostring(v.events), tostring(v.period_us))
  elseif tag == "EVT" then
    info = string.format("EVT %s %s key=%s", who_seq, tostring(v.edge), tostring(v.key))
  elseif tag == "ECH" then
    local rtt = answered and string.format(" (rtt %.3f ms)", (ts - answered.time) * 1000) or ""
    info = string.format("ECH %s echo of %s%s", who_seq, tostring(v.echo_seq), rtt)
  elseif tag == "BAU" then
    info = string.format("BAU %s %s %s baud", who_seq, tostring(v.ack), tostring(v.baud))
  elseif tag == "BST" then
    info = string.format("BST %s burst=%s %s/%s", node_name, seq and tostring(seq) or "?", tostring(v.index), tostring(v.count))
  elseif tag == "BRW" then
    info = string.format("BRW %s browse %s child #%s", who_seq, v.id and node_path(v.id) or "?", tostring(v.index))
  elseif tag == "BRR" then
    local end_of = v.child_id == NONE_ID
    info = string.format("BRR %s req=%s %s%s", who_seq, tostring(v.req),
      end_of and "no more children" or ("child " .. tostring(v.child_id) .. " " .. tostring(v.name) .. " [" .. tostring(v.kind) .. "]"),
      (not end_of and v.more) and " (more)" or "")
  elseif tag == "RED" then
    info = string.format("RED %s read %s", who_seq, v.id and node_path(v.id) or "?")
  elseif tag == "WRT" then
    info = string.format("WRT %s write %s = %s", who_seq, v.id and node_path(v.id) or "?", tostring(v.value))
  elseif tag == "SUB" then
    if v.interval == 0 then
      info = string.format("SUB %s unwatch %s", who_seq, v.id == NONE_ID and "all" or v.id and node_path(v.id) or "?")
    else
      info = string.format("SUB %s watch %s every %s ms", who_seq, v.id and node_path(v.id) or "?", tostring(v.interval))
    end
  elseif tag == "VAL" then
    info = string.format("VAL %s req=%s %s %s = %s", who_seq, tostring(v.req), v.id and node_path(v.id) or "?",
      v.status and (STATUS[v.status] or tostring(v.status)) or "?", tostring(v.value))
  else
    info = string.format("%s %s", tag, who_seq)
  end
  local res = n_seq_result[result_key]
  if res and res.kind == "lost" then info = info .. string.format(" [%s lost]", plural(res.lost, "frame")) end
  if not good then info = info .. " [bad checksum]" end
  if #problems > 0 then info = info .. " [malformed]" end
  item:append_text(": " .. info)
  return info
end

local function is_tcp()
  return f_tcp_srcport() ~= nil
end

--- A UDP datagram is one frame. A TCP segment holds lines, the last of which may be cut.
function node.dissector(tvb, pinfo, tree)
  local len = tvb:len()
  if len == 0 then return 0 end
  local raw = tvb:raw()
  pinfo.cols.protocol = "WSNODE"
  local summaries = {}

  if not is_tcp() then
    local line = len
    while line > 0 and (raw:byte(line) == 10 or raw:byte(line) == 13) do line = line - 1 end
    local ok, info = pcall(dissect_line, tvb, pinfo, tree, 0, line, 1, len)
    summaries[1] = ok and info or ("(error: " .. tostring(info) .. ")")
  else
    local pos, n = 0, 0
    while pos < len do
      local nl = raw:find("\n", pos + 1, true)
      if not nl then
        pinfo.desegment_offset = pos
        pinfo.desegment_len = DESEGMENT_ONE_MORE_SEGMENT
        break
      end
      local line_len = nl - 1 - pos -- without the "\n"
      local wire = line_len + 1
      if line_len > 0 and raw:byte(pos + line_len) == 13 then line_len = line_len - 1 end
      n = n + 1
      local ok, info = pcall(dissect_line, tvb, pinfo, tree, pos, line_len, n, wire)
      summaries[#summaries + 1] = ok and info or ("(error: " .. tostring(info) .. ")")
      pos = nl
    end
  end

  if #summaries > 0 then
    pinfo.cols.info:set(table.concat(summaries, " | "))
  end
  return len
end

-- A heuristic, for a board whose net.port is not the one in the preferences: "$XXX," ... "*CS" in a datagram.
local HEUR_TAGS = {}
for tag in pairs(SCHEMA) do HEUR_TAGS[tag] = true end
local function node_heuristic(tvb, pinfo, tree)
  local len = tvb:len()
  if len < 12 or len > 100 then return false end
  local raw = tvb:raw()
  if raw:byte(1) ~= 36 or raw:byte(5) ~= 44 or not HEUR_TAGS[raw:sub(2, 4)] then return false end
  if not raw:match("%*%x%x[\r\n]*$") then return false end
  node.dissector(tvb, pinfo, tree)
  return true
end
node:register_heuristic("udp", node_heuristic)

local node_ports = { { pref = "port", port = 7000 } }
udp_table:add(7000, node)
tcp_table:add(7000, node)
node.prefs_changed = follow_ports(node, { udp_table, tcp_table }, node_ports)

-- ------------------------------------------------------------------------------------------------
-- wsgen: the traffic generator
-- ------------------------------------------------------------------------------------------------

local gen = Proto("wsgen", "Workshop traffic generator datagram")
gen.prefs.port = Pref.uint("UDP port", 7001, "net.port + 1 of the boards")

local GN = {
  magic = ProtoField.string("wsgen.magic", "Magic"),
  stream = ProtoField.uint16("wsgen.stream", "Stream"),
  flags = ProtoField.uint16("wsgen.flags", "Flags", base.HEX),
  last = ProtoField.bool("wsgen.flags.end", "Last datagram of the stream", 16, nil, 0x0001),
  seq = ProtoField.uint32("wsgen.seq", "Sequence number"),
  sent_us = ProtoField.uint32("wsgen.sent_us", "Sender's clock (µs, low 32 bits)"),
  filler = ProtoField.bytes("wsgen.filler", "Filler"),
  lost = ProtoField.uint32("wsgen.lost", "Datagrams lost before this one"),
  jitter = ProtoField.double("wsgen.jitter_us", "Jitter, RFC 3550 (µs)"),
}
local gnf = {}
for _, f in pairs(GN) do gnf[#gnf + 1] = f end
gen.fields = gnf

local gx_lost = ProtoExpert.new("wsgen.lost_datagrams", "Datagrams lost: a gap in the sequence numbers",
  expert.group.SEQUENCE, expert.severity.WARN)
local gx_order = ProtoExpert.new("wsgen.out_of_order", "Sequence number went back: out of order, or a new stream",
  expert.group.SEQUENCE, expert.severity.NOTE)
local gx_short = ProtoExpert.new("wsgen.short", "Shorter than the 16-byte header",
  expert.group.MALFORMED, expert.severity.ERROR)
local gx_end = ProtoExpert.new("wsgen.end", "End of the stream",
  expert.group.SEQUENCE, expert.severity.CHAT)
gen.experts = { gx_lost, gx_order, gx_short, gx_end }

local g_state  -- "src>dst:stream" -> { last_seq, arrival_us, sent_us, jitter }
local g_result -- frame number -> { kind, lost, jitter }
local function reset_gen() g_state, g_result = {}, {} end
gen.init = reset_gen
reset_gen()

function gen.dissector(tvb, pinfo, tree)
  local len = tvb:len()
  if len < 16 or tvb(0, 4):string() ~= "WGEN" then return 0 end
  pinfo.cols.protocol = "WSGEN"
  local item = tree:add(gen, tvb(0, len))
  item:add(GN.magic, tvb(0, 4))
  local stream = tvb(4, 2):uint()
  item:add(GN.stream, tvb(4, 2))
  local flags = item:add(GN.flags, tvb(6, 2))
  flags:add(GN.last, tvb(6, 2))
  local last = bit32 and bit32.band(tvb(6, 2):uint(), 1) == 1 or (tvb(6, 2):uint() % 2 == 1)
  local seq = tvb(8, 4):uint()
  item:add(GN.seq, tvb(8, 4))
  local sent_us = tvb(12, 4):uint()
  item:add(GN.sent_us, tvb(12, 4))
  if len > 16 then item:add(GN.filler, tvb(16, len - 16)) end

  local res = g_result[pinfo.number]
  if not res then
    local key = tostring(pinfo.src) .. ">" .. tostring(pinfo.dst) .. ":" .. stream
    local st = g_state[key]
    local kind, lost = seq_check(st and st.last_seq, seq)
    local jitter = st and st.jitter or 0
    local arrival_us = pinfo.abs_ts * 1e6
    if st then
      local d_sent = (sent_us - st.sent_us) % TWO32
      if d_sent > 2147483647 then d_sent = d_sent - TWO32 end
      local d = (arrival_us - st.arrival_us) - d_sent
      jitter = jitter + (math.abs(d) - jitter) / 16
    end
    g_state[key] = { last_seq = seq, arrival_us = arrival_us, sent_us = sent_us, jitter = jitter }
    res = { kind = kind, lost = lost, jitter = jitter, has_jitter = st ~= nil }
    g_result[pinfo.number] = res
  end
  if res.kind == "lost" then
    item:add(GN.lost, res.lost):set_generated()
    item:add_proto_expert_info(gx_lost, plural(res.lost, "datagram") .. " lost before this one")
  elseif res.kind == "restart" then
    item:add_proto_expert_info(gx_order)
  end
  if res.has_jitter then item:add(GN.jitter, res.jitter):set_generated() end
  if last then item:add_proto_expert_info(gx_end, "stream " .. stream .. ": " .. plural(seq, "datagram") .. " sent") end

  local info = string.format("WGEN stream %d #%d, %d B%s", stream, seq, len - 16, last and " (end)" or "")
  if res.kind == "lost" then info = info .. string.format(" [%s lost]", plural(res.lost, "datagram")) end
  pinfo.cols.info:set(info)
  item:append_text(": " .. info)
  return len
end

local gen_ports = { { pref = "port", port = 7001 } }
udp_table:add(7001, gen)
gen.prefs_changed = follow_ports(gen, { udp_table }, gen_ports)

-- ------------------------------------------------------------------------------------------------
-- wsadc: the ADC streaming firmware
-- ------------------------------------------------------------------------------------------------

local adc = Proto("wsadc", "Workshop ADC stream")
adc.prefs.port = Pref.uint("UDP port", 7100, "UDP port of the ADC board")

local LOG_LEVELS = { [0] = "Info", "Ok", "Warn", "Error", "Stat", "Reply", "Debug" }
local POT_MODES = { [0] = "off", "freq", "duty" }
local ADC_HEADER = 20
local POT_DATAGRAM = 16

local AD = {
  magic = ProtoField.string("wsadc.magic", "Magic"),
  seq = ProtoField.uint32("wsadc.seq", "Sequence number"),
  rate_mhz = ProtoField.uint32("wsadc.rate_mhz", "Sample rate (mHz)"),
  rate_hz = ProtoField.double("wsadc.rate_hz", "Sample rate (Hz)"),
  dropped = ProtoField.uint32("wsadc.dropped", "Samples lost on the board since start"),
  count = ProtoField.uint16("wsadc.count", "Samples"),
  pin = ProtoField.uint16("wsadc.pin", "GPIO"),
  samples = ProtoField.none("wsadc.samples", "Samples"),
  sample = ProtoField.uint16("wsadc.sample", "Sample (raw code)"),
  min = ProtoField.uint16("wsadc.min", "Smallest sample"),
  max = ProtoField.uint16("wsadc.max", "Largest sample"),
  mean = ProtoField.double("wsadc.mean", "Mean sample"),
  lost = ProtoField.uint32("wsadc.lost", "Datagrams lost before this one"),
  -- log
  ms = ProtoField.uint32("wsadc.ms", "Board uptime (ms)"),
  level = ProtoField.uint8("wsadc.level", "Level", base.DEC, LOG_LEVELS),
  text = ProtoField.string("wsadc.text", "Text", base.UNICODE),
  -- potentiometer
  pot_mode = ProtoField.uint8("wsadc.pot.mode", "Mode", base.DEC, POT_MODES),
  pot_position = ProtoField.uint16("wsadc.pot.position", "Knob position (1/100 % of the travel)"),
  pot_code = ProtoField.uint16("wsadc.pot.code", "Averaged ADC code"),
  pot_value = ProtoField.float("wsadc.pot.value", "Value given to the generator (Hz or %)"),
  -- commands and the answer to start
  command = ProtoField.string("wsadc.command", "Command"),
  reply = ProtoField.string("wsadc.reply", "Answer"),
  kv_rate = ProtoField.uint32("wsadc.start.rate", "Asked sample rate (Hz)"),
  kv_n = ProtoField.uint32("wsadc.start.n", "Samples per datagram"),
  ok_rate_mhz = ProtoField.uint32("wsadc.ok.rate_mhz", "Exact sample rate (mHz)"),
  ok_n = ProtoField.uint32("wsadc.ok.n", "Samples per datagram"),
  ok_pin = ProtoField.uint32("wsadc.ok.pin", "GPIO"),
  ok_zero = ProtoField.uint32("wsadc.ok.zero", "Code read at 0 V"),
  ok_full = ProtoField.uint32("wsadc.ok.full", "Full-scale code"),
  ok_verbose = ProtoField.uint32("wsadc.ok.verbose", "Verbose"),
  ok_pot_pin = ProtoField.uint32("wsadc.ok.pot_pin", "Potentiometer GPIO"),
}
local adf = {}
for _, f in pairs(AD) do adf[#adf + 1] = f end
adc.fields = adf

local ax_lost = ProtoExpert.new("wsadc.lost_datagrams", "Datagrams lost: a gap in the sequence numbers",
  expert.group.SEQUENCE, expert.severity.WARN)
local ax_restart = ProtoExpert.new("wsadc.restart", "Sequence number went back: a new stream (start) was requested",
  expert.group.SEQUENCE, expert.severity.CHAT)
local ax_dropped = ProtoExpert.new("wsadc.dropped_samples", "The board lost samples (ring or DMA full)",
  expert.group.SEQUENCE, expert.severity.NOTE)
local ax_size = ProtoExpert.new("wsadc.size", "Datagram length does not match the sample count",
  expert.group.MALFORMED, expert.severity.ERROR)
local ax_log_warn = ProtoExpert.new("wsadc.log_warn", "The board logged a warning",
  expert.group.RESPONSE_CODE, expert.severity.WARN)
local ax_log_error = ProtoExpert.new("wsadc.log_error", "The board logged an error",
  expert.group.RESPONSE_CODE, expert.severity.ERROR)
adc.experts = { ax_lost, ax_restart, ax_dropped, ax_size, ax_log_warn, ax_log_error }

local a_last  -- "src>dst" -> last seq
local a_result -- frame number -> { kind, lost }
local function reset_adc() a_last, a_result = {}, {} end
adc.init = reset_adc
reset_adc()

local function is_text(s)
  return #s > 0 and not s:find("[^\t\n\r\32-\126]")
end

local function dissect_samples(tvb, pinfo, item, len)
  local count = tvb(16, 2):le_uint()
  local pin = tvb(18, 2):le_uint()
  local seq = tvb(4, 4):le_uint()
  local rate_mhz = tvb(8, 4):le_uint()
  local dropped = tvb(12, 4):le_uint()

  item:add_le(AD.seq, tvb(4, 4))
  item:add_le(AD.rate_mhz, tvb(8, 4))
  item:add(AD.rate_hz, tvb(8, 4), rate_mhz / 1000):set_generated()
  local dropped_item = item:add_le(AD.dropped, tvb(12, 4))
  item:add_le(AD.count, tvb(16, 2))
  item:add_le(AD.pin, tvb(18, 2))
  if dropped > 0 then dropped_item:add_proto_expert_info(ax_dropped) end

  local available = math.floor((tvb:len() - ADC_HEADER) / 2)
  if tvb:len() ~= ADC_HEADER + 2 * count then
    item:add_proto_expert_info(ax_size, string.format("%d bytes for %d samples (%d expected)", tvb:len(), count, ADC_HEADER + 2 * count))
    count = math.min(count, available)
  end
  if count > 0 then
    local list = item:add(AD.samples, tvb(ADC_HEADER, 2 * count)):append_text(string.format(" (%d × u16)", count))
    local lo, hi, sum = 65535, 0, 0
    for i = 0, count - 1 do
      local rng = tvb(ADC_HEADER + 2 * i, 2)
      local sample = rng:le_uint()
      list:add_le(AD.sample, rng)
      if sample < lo then lo = sample end
      if sample > hi then hi = sample end
      sum = sum + sample
    end
    item:add(AD.min, lo):set_generated()
    item:add(AD.max, hi):set_generated()
    item:add(AD.mean, sum / count):set_generated()
  end

  local res = a_result[pinfo.number]
  if not res then
    local key = tostring(pinfo.src) .. ">" .. tostring(pinfo.dst)
    local kind, lost = seq_check(a_last[key], seq)
    res = { kind = kind, lost = lost }
    a_result[pinfo.number] = res
    a_last[key] = seq
  end
  if res.kind == "lost" then
    item:add(AD.lost, res.lost):set_generated()
    item:add_proto_expert_info(ax_lost, plural(res.lost, "datagram") .. " lost before this one")
  elseif res.kind == "restart" then
    item:add_proto_expert_info(ax_restart)
  end
  local info = string.format("ADC1 #%d  %.1f Hz  %d samples  GPIO%d", seq, rate_mhz / 1000, count, pin)
  if dropped > 0 then info = info .. string.format("  dropped %d", dropped) end
  if res.kind == "lost" then info = info .. string.format("  [%s lost]", plural(res.lost, "datagram")) end
  return info
end

local function dissect_log(tvb, pinfo, item, magic)
  local seq = tvb(4, 4):le_uint()
  local level = tvb(12, 1):uint()
  local text = tvb(13, tvb:len() - 13):string(ENC_UTF_8)
  item:add_le(AD.seq, tvb(4, 4))
  item:add_le(AD.ms, tvb(8, 4))
  local lv = item:add(AD.level, tvb(12, 1))
  if level == 2 then lv:add_proto_expert_info(ax_log_warn, text) end
  if level == 3 then lv:add_proto_expert_info(ax_log_error, text) end
  item:add(AD.text, tvb(13, tvb:len() - 13), text)
  local label = magic == "RPL1" and "answer" or "log"
  return string.format("%s %s%s: %s", magic, label, magic == "LOG1" and (" #" .. seq) or "", text), text
end

local function dissect_pot(tvb, item)
  local mode = tvb(4, 1):uint()
  local position = tvb(6, 2):le_uint()
  local value = tvb(12, 4):le_float()
  item:add(AD.pot_mode, tvb(4, 1))
  item:add_le(AD.pot_position, tvb(6, 2))
  item:add_le(AD.pot_code, tvb(8, 2))
  item:add_le(AD.pot_value, tvb(12, 4))
  return string.format("POT1 %s  knob %.2f %%  → %.4g %s", POT_MODES[mode] or "?", position / 100, value,
    mode == 2 and "%" or "Hz")
end

local function key_values(item, tvb, text, fields)
  for key, value in text:gmatch("([%w_]+)=(%d+)") do
    local pf = fields[key]
    if pf then
      local pos = text:find(key .. "=" .. value, 1, true)
      item:add(pf, tvb(pos - 1, #key + 1 + #value), tonumber(value))
    end
  end
end

function adc.dissector(tvb, pinfo, tree)
  local len = tvb:len()
  if len == 0 then return 0 end
  pinfo.cols.protocol = "WSADC"
  local item = tree:add(adc, tvb(0, len))
  local magic = len >= 4 and tvb(0, 4):string() or ""
  local info

  if magic == "ADC1" and len >= ADC_HEADER then
    item:add(AD.magic, tvb(0, 4))
    info = dissect_samples(tvb, pinfo, item, len)
  elseif magic == "POT1" and len >= POT_DATAGRAM then
    item:add(AD.magic, tvb(0, 4))
    info = dissect_pot(tvb, item)
  elseif (magic == "LOG1" or magic == "RPL1") and len >= 13 then
    item:add(AD.magic, tvb(0, 4))
    info = dissect_log(tvb, pinfo, item, magic)
  else
    local text = tvb:raw()
    if not is_text(text) then
      pinfo.cols.info:set("(not an ADC stream datagram)")
      return len
    end
    text = text:gsub("[\r\n]+$", "")
    local to_board = pinfo.dst_port == adc.prefs.port
    if to_board then
      item:add(AD.command, tvb(0, #text))
      key_values(item, tvb, text, { rate = AD.kv_rate, n = AD.kv_n })
      info = "command: " .. text
    else
      item:add(AD.reply, tvb(0, #text))
      key_values(item, tvb, text, {
        rate_mhz = AD.ok_rate_mhz, n = AD.ok_n, pin = AD.ok_pin,
        zero = AD.ok_zero, full = AD.ok_full, verbose = AD.ok_verbose, pot_pin = AD.ok_pot_pin,
      })
      info = "answer: " .. text
    end
  end
  pinfo.cols.info:set(info)
  item:append_text(": " .. info)
  return len
end

local adc_ports = { { pref = "port", port = 7100 } }
udp_table:add(7100, adc)
adc.prefs_changed = follow_ports(adc, { udp_table }, adc_ports)
