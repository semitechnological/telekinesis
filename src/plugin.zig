const std = @import("std");
const agent = @import("agent.zig");

const log = std.log.scoped(.plugin);

pub const PluginId = []const u8;

pub const RpcMessage = struct {
    id: u64,
    type: []const u8,
    command: ?[]const u8 = null,
    success: ?bool = null,
    event: ?[]const u8 = null,
    name: ?[]const u8 = null,
    description: ?[]const u8 = null,
    parameters: ?[]const u8 = null,
    arguments: ?[]const u8 = null,
    content: ?[]const u8 = null,
    result: ?[]const u8 = null,
    is_error: ?bool = null,
};

pub const RegisteredTool = struct {
    name: []const u8,
    description: []const u8,
    parameters_json: []const u8,
    plugin_id: []const u8,
};

pub const Plugin = struct {
    allocator: std.mem.Allocator,
    io: std.Io = .global,
    id: []const u8,
    name: []const u8,
    path: []const u8,
    process: ?std.process.Child = null,
    next_rpc_id: u64 = 1,
    registered_tools: std.ArrayList(RegisteredTool),

    pub fn init(allocator: std.mem.Allocator, id: []const u8, name: []const u8, path: []const u8) !Plugin {
        return .{
            .allocator = allocator,
            .id = try allocator.dupe(u8, id),
            .name = try allocator.dupe(u8, name),
            .path = try allocator.dupe(u8, path),
            .registered_tools = std.ArrayList(RegisteredTool).empty,
        };
    }

    pub fn deinit(self: *Plugin) void {
        self.kill();
        self.allocator.free(self.id);
        self.allocator.free(self.name);
        self.allocator.free(self.path);
        for (self.registered_tools.items) |tool| {
            self.allocator.free(tool.name);
            self.allocator.free(tool.description);
            self.allocator.free(tool.parameters_json);
            self.allocator.free(tool.plugin_id);
        }
        self.registered_tools.deinit(self.allocator);
    }

    pub fn start(self: *Plugin, io: std.Io) !void {
        self.io = io;
        var child = std.process.Child.init(io);
        child.argv = &.{ "bun", "run", self.path };
        child.stdin_behavior = .Pipe;
        child.stdout_behavior = .Pipe;
        child.stderr_behavior = .Inherit;
        try child.spawn();
        self.process = child;
        log.info("started plugin {s}: bun run {s}", .{ self.id, self.path });
    }

    pub fn kill(self: *Plugin) void {
        if (self.process) |*p| {
            p.kill(self.io);
            _ = p.wait(self.io) catch {};
            self.process = null;
        }
    }

    pub fn sendRpc(self: *Plugin, msg: RpcMessage) !void {
        if (self.process == null) return error.PluginNotRunning;
        const file = self.process.?.stdin orelse return error.PluginNotRunning;

        var buf: [4096]u8 = undefined;
        var file_writer: std.Io.File.Writer = .init(file, .global, &buf);
        const writer = &file_writer.interface;

        try std.json.Stringify.value(msg, .{}, writer);
        try writer.writeAll("\n");
        try writer.flush();
    }

    pub fn nextId(self: *Plugin) u64 {
        const id = self.next_rpc_id;
        self.next_rpc_id += 1;
        return id;
    }

    pub fn registerTool(self: *Plugin, name: []const u8, description: []const u8, parameters_json: []const u8) !void {
        try self.registered_tools.append(self.allocator, .{
            .name = try self.allocator.dupe(u8, name),
            .description = try self.allocator.dupe(u8, description),
            .parameters_json = try self.allocator.dupe(u8, parameters_json),
            .plugin_id = try self.allocator.dupe(u8, self.id),
        });
        log.info("plugin {s} registered tool: {s}", .{ self.id, name });
    }

    pub fn callTool(self: *Plugin, name: []const u8, arguments: []const u8) !agent.ToolResult {
        const rpc_id = self.nextId();
        try self.sendRpc(.{
            .id = rpc_id,
            .type = "tool_call",
            .name = name,
            .arguments = arguments,
        });
        return .{
            .id = "plugin-tool",
            .content = try self.allocator.dupe(u8, "[plugin tool execution pending]"),
            .is_error = false,
        };
    }
};

pub const Registry = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    plugins: std.ArrayList(Plugin),

    pub fn init(allocator: std.mem.Allocator, io: std.Io) Registry {
        return .{
            .allocator = allocator,
            .io = io,
            .plugins = std.ArrayList(Plugin).empty,
        };
    }

    pub fn deinit(self: *Registry) void {
        for (self.plugins.items) |*plugin| {
            plugin.deinit();
        }
        self.plugins.deinit(self.allocator);
    }

    pub fn add(self: *Registry, id: []const u8, name: []const u8, path: []const u8) !void {
        try self.plugins.append(self.allocator, try Plugin.init(self.allocator, id, name, path));
        log.info("loaded plugin: {s}", .{id});
    }

    pub fn startAll(self: *Registry) !void {
        for (self.plugins.items) |*plugin| {
            try plugin.start(self.io);
        }
    }

    pub fn stopAll(self: *Registry) void {
        for (self.plugins.items) |*plugin| {
            plugin.kill();
        }
    }

    pub fn count(self: *const Registry) usize {
        return self.plugins.items.len;
    }

    pub fn get(self: *const Registry, id: []const u8) ?*const Plugin {
        for (self.plugins.items) |*plugin| {
            if (std.mem.eql(u8, plugin.id, id)) return plugin;
        }
        return null;
    }

    pub fn allTools(self: *const Registry, allocator: std.mem.Allocator) ![]RegisteredTool {
        var result: std.ArrayList(RegisteredTool) = .empty;
        for (self.plugins.items) |plugin| {
            for (plugin.registered_tools.items) |tool| {
                try result.append(allocator, tool);
            }
        }
        return result.toOwnedSlice(allocator);
    }
};

pub const Skill = struct {
    name: []const u8,
    description: []const u8,
    path: []const u8,
    disable_model_invocation: bool = false,
};

pub const SkillLoader = struct {
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator) SkillLoader {
        return .{ .allocator = allocator };
    }

    pub fn loadFromDir(self: *SkillLoader, dir_path: []const u8) ![]Skill {
        var skills: std.ArrayList(Skill) = .empty;
        self.scanDir(&skills, dir_path) catch |err| {
            log.warn("skill dir {s}: {}", .{ dir_path, err });
        };
        return skills.toOwnedSlice(self.allocator);
    }

    fn scanDir(self: *SkillLoader, skills: *std.ArrayList(Skill), dir_path: []const u8) !void {
        var dir = std.fs.cwd().openDir(dir_path, .{ .iterate = true }) catch return;
        defer dir.close();

        var iter = dir.iterate();
        while (try iter.next()) |entry| {
            if (entry.kind == .file and std.mem.eql(u8, entry.name, "SKILL.md")) {
                const skill = try self.parseSkillFile(dir_path, entry.name);
                try skills.append(self.allocator, skill);
            } else if (entry.kind == .directory) {
                const sub_path = try std.fmt.allocPrint(self.allocator, "{s}/{s}", .{ dir_path, entry.name });
                defer self.allocator.free(sub_path);
                const skill_file = try std.fmt.allocPrint(self.allocator, "{s}/SKILL.md", .{sub_path});
                defer self.allocator.free(skill_file);
                if (std.fs.cwd().access(skill_file, .{})) {
                    try skills.append(self.allocator, try self.parseSkillFile(sub_path, "SKILL.md"));
                } else |_| {
                    try self.scanDir(skills, sub_path);
                }
            }
        }
    }

    fn parseSkillFile(self: *SkillLoader, dir_path: []const u8, filename: []const u8) !Skill {
        const full_path = try std.fmt.allocPrint(self.allocator, "{s}/{s}", .{ dir_path, filename });
        const file = try std.fs.cwd().openFile(full_path, .{});
        defer file.close();
        const content = try file.readToEndAlloc(self.allocator, 1024 * 1024);
        defer self.allocator.free(content);

        var name: []const u8 = try self.allocator.dupe(u8, "unnamed");
        var description: []const u8 = try self.allocator.dupe(u8, "");
        var disable = false;

        if (std.mem.startsWith(u8, content, "---")) {
            const end = std.mem.indexOf(u8, content[3..], "---") orelse return .{
                .name = name,
                .description = description,
                .path = full_path,
                .disable_model_invocation = disable,
            };
            const frontmatter = content[3 .. 3 + end];
            var line_iter = std.mem.splitScalar(u8, frontmatter, '\n');
            while (line_iter.next()) |line| {
                const trimmed = std.mem.trim(u8, line, " \r\t");
                if (std.mem.startsWith(u8, trimmed, "name:")) {
                    self.allocator.free(name);
                    name = try self.allocator.dupe(u8, std.mem.trim(u8, trimmed[5..], " "));
                } else if (std.mem.startsWith(u8, trimmed, "description:")) {
                    self.allocator.free(description);
                    description = try self.allocator.dupe(u8, std.mem.trim(u8, trimmed[12..], " "));
                } else if (std.mem.startsWith(u8, trimmed, "disable-model-invocation:")) {
                    const val = std.mem.trim(u8, trimmed[25..], " ");
                    disable = std.mem.eql(u8, val, "true");
                }
            }
        }

        return .{
            .name = name,
            .description = description,
            .path = full_path,
            .disable_model_invocation = disable,
        };
    }

    pub fn buildSystemPrompt(self: *SkillLoader, skills: []const Skill) ![]u8 {
        var buf: std.Io.Writer.Allocating = .init(self.allocator);
        const writer = &buf.writer;
        defer buf.deinit();

        try writer.writeAll("<available_skills>\n");
        for (skills) |skill| {
            try writer.print(
                \\  <skill>
                \\    <name>{s}</name>
                \\    <description>{s}</description>
                \\    <location>{s}</location>
                \\  </skill>
                \\
            , .{ skill.name, skill.description, skill.path });
        }
        try writer.writeAll("</available_skills>");
        return try self.allocator.dupe(u8, buf.written());
    }
};

test "plugin registry loads plugin" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    var registry = Registry.init(gpa, io);
    defer registry.deinit();
    try registry.add("test", "Test Plugin", "./plugins/test");
    try std.testing.expectEqual(@as(usize, 1), registry.count());
    try std.testing.expect(registry.get("test") != null);
}

test "plugin register tool" {
    const gpa = std.testing.allocator;
    var plugin = try Plugin.init(gpa, "test", "Test", "./test.ts");
    defer plugin.deinit();
    try plugin.registerTool("read_file", "Read a file", "{}");
    try std.testing.expectEqual(@as(usize, 1), plugin.registered_tools.items.len);
    try std.testing.expectEqualStrings("read_file", plugin.registered_tools.items[0].name);
}

test "skill loader parses frontmatter" {
    const gpa = std.testing.allocator;
    const tmp_dir = ".telekinesis-test-skills";
    defer std.fs.cwd().deleteTree(tmp_dir) catch {};

    try std.fs.cwd().makePath(tmp_dir);
    const skill_content =
        \\---
        \\name: test-skill
        \\description: A test skill
        \\disable-model-invocation: true
        \\---
        \\# Test Skill
        \\This is a test.
    ;
    const skill_path = try std.fmt.allocPrint(gpa, "{s}/SKILL.md", .{tmp_dir});
    defer gpa.free(skill_path);
    var file = try std.fs.cwd().createFile(skill_path, .{});
    try file.writeAll(skill_content);
    file.close();

    var loader = SkillLoader.init(gpa);
    const skills = try loader.loadFromDir(tmp_dir);
    defer {
        for (skills) |s| {
            gpa.free(s.name);
            gpa.free(s.description);
            gpa.free(s.path);
        }
        gpa.free(skills);
    }

    try std.testing.expectEqual(@as(usize, 1), skills.len);
    try std.testing.expectEqualStrings("test-skill", skills[0].name);
    try std.testing.expect(skills[0].disable_model_invocation);
}

test "skill loader builds system prompt" {
    const gpa = std.testing.allocator;
    var loader = SkillLoader.init(gpa);
    const skills = [_]Skill{
        .{ .name = "skill-a", .description = "desc a", .path = "/path/a" },
        .{ .name = "skill-b", .description = "desc b", .path = "/path/b" },
    };
    const prompt = try loader.buildSystemPrompt(&skills);
    defer gpa.free(prompt);

    try std.testing.expect(std.mem.indexOf(u8, prompt, "<available_skills>") != null);
    try std.testing.expect(std.mem.indexOf(u8, prompt, "skill-a") != null);
    try std.testing.expect(std.mem.indexOf(u8, prompt, "skill-b") != null);
}
