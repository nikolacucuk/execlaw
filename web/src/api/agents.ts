import { apiFetch } from "./client";

export interface AgentView { id:string; name:string; role_prompt:string; model:string|null; backend_purpose:string; tools:string[]; trust_policy:Record<string, unknown>; trigger:Record<string, unknown>; reply_mode:"draft"|"automatic"; interval_secs:number; token_budget:number; max_runtime_secs:number; concurrency_limit:number; enabled:boolean; paused:boolean; next_run_at:number|null; last_run_at:number|null; last_run_status:string|null; last_error:string|null; }
export interface AgentRun { id:string; agent_id:string; status:string; started_at:number; finished_at:number|null; tokens_used:number|null; checkpoint:Record<string, unknown>; output_text:string|null; error:string|null; }
export interface AgentRequest { id?:string; name:string; role_prompt:string; model?:string|null; backend_purpose?:string; tools?:string[]; trust_policy?:Record<string, unknown>; trigger?:Record<string, unknown>; reply_mode?:"draft"|"automatic"; interval_secs?:number; token_budget?:number; max_runtime_secs?:number; concurrency_limit?:number; enabled?:boolean; }
export function listAgents(token:()=>string|null){return apiFetch<AgentView[]>("/api/admin/agents",{},token)}
export function createAgent(body:AgentRequest,token:()=>string|null){return apiFetch<AgentView>("/api/admin/agents",{method:"POST",body},token)}
export function importAgentMarkdown(markdown:string,token:()=>string|null){return apiFetch<AgentView>("/api/admin/agents/import-markdown",{method:"POST",body:{markdown}},token)}
export function pauseAgent(id:string,token:()=>string|null){return apiFetch<boolean>(`/api/admin/agents/${encodeURIComponent(id)}/pause`,{method:"POST"},token)}
export function resumeAgent(id:string,token:()=>string|null){return apiFetch<boolean>(`/api/admin/agents/${encodeURIComponent(id)}/resume`,{method:"POST"},token)}
export function sendAgentMessage(id:string,content:string,token:()=>string|null){return apiFetch<string>(`/api/admin/agents/${encodeURIComponent(id)}/messages`,{method:"POST",body:{content}},token)}
export function listAgentRuns(id:string,token:()=>string|null){return apiFetch<AgentRun[]>(`/api/admin/agents/${encodeURIComponent(id)}/runs`,{},token)}
