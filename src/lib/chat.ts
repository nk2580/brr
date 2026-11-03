import { invoke } from '@tauri-apps/api/core'
import type { DraftDocument } from './chat-draft'
import type { Thread } from './client'
import type { Task, TaskInput } from './agents'
export type ChatSettings={model:string;effort:string;mode:'default'|'plan';readOnly?:boolean;modelId?:string}
export type SavedDraft={id:string;threadId?:string|null;projectId?:string|null;agentId?:string|null;document:DraftDocument;settings:ChatSettings;revision:number;updatedAt:string;branchEdit?:true;branchOperationId?:string;branchThread?:Thread}
export type ChatAttachment={id:string;name:string;mime:string;size:number;kind:'image'|'file';status:'ready'}
export type ChatSubmission={id:string;threadId:string;document:DraftDocument;attachmentIds:string[];settings:ChatSettings;delivery:'send'|'queue'|'steer';expectedTurnId?:string;status:'queued'|'dispatching'|'accepted'|'uncertain'|'failed'|'cancelled';turnId?:string;error?:string;revision:number}
export type RecoveryItem={sourceId:string;sourceKind:'submission'|'draft';threadId:string;kind:'baseline'|'agent';projectId:string|null;agentId:string|null;status:'queued'|'failed'|'uncertain'|'draft'|string;text:string;settings:ChatSettings;attachmentIds:string[];createdAt:string;revision:number;restorable:boolean;reason?:string}
export type RecoveryRestoreResult={restored:true;target:'draft';draft:SavedDraft;attachments:ChatAttachment[]}|{restored:true;target:'chat';threadId:string}
export type QueueSnapshot={entries:ChatSubmission[];state:{paused:boolean;reason?:string}}
export type ReferenceScope={threadId:string;projectId?:never}|{projectId:string;threadId?:never}
const request=<T>(method:string,params:Record<string,unknown>={})=>invoke<T>('chat_request',{method,params})
export const chat={
 assignTask:(params:{ownerId:string;document:DraftDocument;attachmentIds:string[];task:TaskInput & {sourceThreadId?:string;operationId?:string}})=>request<Task>('tasks.assign',params),
 listDrafts:()=>request<SavedDraft[]>('drafts.list'),
 getDraft:(id:string)=>request<SavedDraft>('drafts.get',{id}),
 saveDraft:(draft:Omit<SavedDraft,'updatedAt'|'revision'>,expectedRevision:number)=>request<SavedDraft>('drafts.save',{...draft,expectedRevision}),
 promoteDraft:(id:string,threadId:string)=>request<SavedDraft>('drafts.promote',{id,threadId}),
 deleteDraft:(id:string)=>request('drafts.delete',{id}),
 attachments:(ownerId:string)=>request<ChatAttachment[]>('attachments.list',{ownerId}),
 importPaths:(ownerId:string,paths:string[])=>request<ChatAttachment[]>('attachments.import',{ownerId,paths}),
 async importFiles(ownerId:string,files:File[]){
  const encoded=await Promise.all(files.map(async file=>{const bytes=new Uint8Array(await file.arrayBuffer());let text='';for(let i=0;i<bytes.length;i+=16384)text+=String.fromCharCode(...bytes.subarray(i,i+16384));return {name:file.name,mime:file.type||'application/octet-stream',data:btoa(text)}}))
  return request<ChatAttachment[]>('attachments.importBytes',{ownerId,files:encoded})
 },
 preview:(ownerId:string,id:string)=>request<{attachment:ChatAttachment;preview:string|null;reason?:string}>('attachments.preview',{ownerId,id}),
 removeAttachment:(ownerId:string,id:string)=>request('attachments.remove',{ownerId,id}),
 submit:(params:{id?:string;threadId:string;document:DraftDocument;attachmentIds?:string[];settings:ChatSettings;delivery:'send'|'queue'|'steer';expectedTurnId?:string})=>request<{turn?:{id:string};turnId?:string;submission:ChatSubmission;queued:boolean}>('foreground.submit',params),
 queue:(threadId:string)=>request<QueueSnapshot>('queue.list',{threadId}),
 pause:(threadId:string)=>request('queue.pause',{threadId,reason:'Paused by you'}),
 resume:(threadId:string)=>request('queue.resume',{threadId}),
 removeQueued:(id:string)=>request('queue.remove',{id}),
 editQueued:(id:string,text:string)=>request('queue.edit',{id,text}),
 steerQueued:(id:string)=>request('queue.steer',{id}),
 resolveReference:(scope:ReferenceScope,id:string,type:'file'|'folder'='file')=>request<{root:string;path:string;absolutePath:string}>('references.resolve',{...scope,id,type}),
 references:(params:ReferenceScope & {query:string})=>request<Array<{id:string;name:string;path:string;type:'file'|'folder'}>>('references.search',params),
 async skills(cwd:string){const response=await invoke<{data:Array<{skills:Array<{name:string;path:string;description?:string;enabled?:boolean}>}>}>('engine_request',{method:'skills/list',params:{cwds:cwd?[cwd]:[]}});return response.data.flatMap(group=>group.skills??[]).filter(skill=>skill.enabled!==false)},
 pendingBranches:()=>request<{data:Array<{id:string;sourceThreadId:string;threadId?:string;phase:string;error?:string;cwd?:string}>}>('branches.pending'),
 recoverBranch:(operationId:string)=>request<{thread:Thread}>('branches.recover',{operationId}),
 recovery:{
  list:()=>request<{items:RecoveryItem[]}>('recovery.list'),
  restore:(params:{sourceId:string;sourceKind:RecoveryItem['sourceKind'];operationId:string})=>request<RecoveryRestoreResult>('recovery.restore',params),
  cancel:(params:{submissionId:string;expectedRevision:number})=>request('recovery.cancel',params),
 },
 branch:(threadId:string,turnId:string,before=false)=>request<{thread:Thread;draft?:SavedDraft}>('branches.create',{threadId,turnId,before,operationId:crypto.randomUUID()}),
}
