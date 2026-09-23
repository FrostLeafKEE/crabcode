import { invoke } from "@tauri-apps/api/core";

export interface LocalVmConfig {
  name: string;
  user: string;
  storage: string;
  shared_directory: string;
  shared_read_only: boolean;
  forwarded_ports: number[];
}
export const DEFAULT_VM_CONFIG: LocalVmConfig = {
  name: "crabcode", user: "lume", storage: "default", shared_directory: "", shared_read_only: true, forwarded_ports: [],
};
export function normalizeVmConfig(raw: Partial<LocalVmConfig> | null | undefined): LocalVmConfig {
  const value = raw ?? {};
  const string = (key: keyof LocalVmConfig): string => typeof value[key] === "string" ? String(value[key]) : String(DEFAULT_VM_CONFIG[key]);
  return { name: string("name"), user: string("user"), storage: string("storage"), shared_directory: string("shared_directory"), shared_read_only: value.shared_read_only !== false, forwarded_ports: Array.isArray(value.forwarded_ports) ? value.forwarded_ports.filter(p => Number.isInteger(p) && p > 0 && p <= 65535).slice(0, 8) : [] };
}
export function vmEnvironmentId(config: LocalVmConfig): string { return `lume:${config.storage}:${config.name}`; }
export function vmHostId(base: string, config: LocalVmConfig): string {
  // A distinct host route prevents an old session from being redirected when
  // the selected VM or account changes. Include configuration to fence edits.
  const source = JSON.stringify(config);
  let a = 2166136261, b = 5381;
  for (const character of source) { a = Math.imul(a ^ character.charCodeAt(0), 16777619); b = Math.imul(b, 33) ^ character.charCodeAt(0); }
  return `${base}-vm-${(a >>> 0).toString(16)}${(b >>> 0).toString(16)}`;
}
export interface VmInfo { name: string; status: string; os?: string }
export function listVirtualMachines(storage: string): Promise<VmInfo[]> {
  return invoke("computer_use_vm_list", { storage });
}
export function manageVirtualMachine(config: LocalVmConfig, operation: string, options?: {
  password?: string;
  create?: { cpus: number; memory_gb: number; disk_gb: number; ipsw: string };
}): Promise<{ ok: boolean; error?: string; summary?: string }> {
  return invoke("computer_use_vm_manage", { config, operation, password: options?.password ?? null, create: options?.create ?? null });
}
