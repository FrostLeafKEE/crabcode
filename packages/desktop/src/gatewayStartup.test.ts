import { describe, expect, it } from "vitest";
import { gatewayEnvironmentLog, gatewayLogAddress } from "./gatewayStartup";

describe("Gateway environment logs", () => {
  it("uses the running Gateway's environment, including remote interpreter paths", () => {
    const lines = gatewayEnvironmentLog({
      startup_cwd: "/srv/project", home: "/home/service", browse_roots: [],
      runtime: {
        gateway_version: "0.1.5", gateway_path: "/srv/venv/lib/crabcode_gateway",
        python_version: "3.12.9", python_executable: "/srv/venv/bin/python",
        python_prefix: "/srv/venv", environment_kind: "venv", platform: "Linux x86_64",
      },
    });
    expect(lines.join("\n")).toContain("Python 解释器路径：/srv/venv/bin/python");
    expect(lines.join("\n")).toContain("Python 环境：虚拟环境 · /srv/venv");
    expect(lines.join("\n")).toContain("Gateway 使用的 Python：3.12.9");
    expect(lines.join("\n")).toContain("Gateway 启动目录：/srv/project");
  });

  it("handles old Gateways without inventing local runtime details", () => {
    const lines = gatewayEnvironmentLog({ startup_cwd: "/srv/project", home: "/home/service", browse_roots: [] });
    expect(lines).toHaveLength(2);
    expect(lines[0]).toContain("未提供详细运行环境信息");
    expect(lines.join("\n")).not.toContain("python3");
  });

  it("omits credentials and URL parameters from connection logs", () => {
    expect(gatewayLogAddress("https://user:secret@example.com/gateway?token=private#secret"))
      .toBe("https://example.com/gateway");
  });
});
