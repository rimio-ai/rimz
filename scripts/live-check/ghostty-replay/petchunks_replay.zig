const std = @import("std");
const Terminal = @import("../Terminal.zig");
const apc = @import("../apc.zig");
const stream = @import("../stream.zig");
const readonly = @import("../stream_readonly.zig");

pub fn run(result: *const []const u8) !void {
    const alloc = std.heap.smp_allocator;
    const input = try std.process.getEnvVarOwned(alloc, "PETCHUNKS_INPUT");
    defer alloc.free(input);
    const output = try std.process.getEnvVarOwned(alloc, "PETCHUNKS_OUTPUT");
    defer alloc.free(output);
    const bytes = try std.fs.cwd().readFileAlloc(alloc, input, 100 * 1024 * 1024);
    defer alloc.free(bytes);
    var file = try std.fs.cwd().createFile(output, .{});
    defer file.close();
    var terminal = try Terminal.init(alloc, .{ .rows = 50, .cols = 200 });
    defer terminal.deinit(alloc);
    const H = struct {
        terminal: *Terminal,
        alloc: std.mem.Allocator,
        apc_handler: apc.Handler = .{},
        file: std.fs.File,
        result: *const []const u8,
        commands: usize = 0,
        errors: usize = 0,
        pub fn deinit(self: *@This()) void {
            self.apc_handler.deinit();
        }
        pub fn vt(self: *@This(), comptime action: stream.Action.Tag, value: stream.Action.Value(action)) !void {
            switch (action) {
                .apc_start => self.apc_handler.start(),
                .apc_put => self.apc_handler.feed(self.alloc, value),
                .apc_end => {
                    var cmd = self.apc_handler.end() orelse return;
                    defer cmd.deinit(self.alloc);
                    switch (cmd) {
                        .kitty => |*kitty_cmd| {
                            _ = self.terminal.kittyGraphics(self.alloc, kitty_cmd);
                            self.commands += 1;
                            const message = self.result.*;
                            if (!std.mem.startsWith(u8, message, "OK")) self.errors += 1;
                            var buf: [1024]u8 = undefined;
                            const line = try std.fmt.bufPrint(&buf, "command={d} action={s} result={s}\n", .{ self.commands, @tagName(kitty_cmd.control), message });
                            try self.file.writeAll(line);
                        },
                    }
                },
                else => {
                    var handler = readonly.Handler.init(self.terminal);
                    try handler.vt(action, value);
                },
            }
        }
    };
    var parser = stream.Stream(H).initAlloc(alloc, .{ .terminal = &terminal, .alloc = alloc, .file = file, .result = result });
    defer parser.deinit();
    try parser.nextSlice(bytes);
    var buf: [256]u8 = undefined;
    var total: usize = 0;
    var ids = terminal.screens.active.kitty_images.images.keyIterator();
    while (ids.next()) |id| {
        try file.writeAll(try std.fmt.bufPrint(&buf, "image={d}\n", .{id.*}));
        total += 1;
    }
    try file.writeAll(try std.fmt.bufPrint(&buf, "SUMMARY commands={d} errors={d} images={d}\n", .{ parser.handler.commands, parser.handler.errors, total }));
}
