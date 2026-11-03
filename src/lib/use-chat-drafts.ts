import {useCallback,useEffect,useRef,useState} from 'react'
import {chat,type SavedDraft,type ChatSettings} from './chat'
import {emptyDraft,type DraftDocument} from './chat-draft'
// One serialized writer prevents a slow old save from replacing a newer draft.
export function useChatDrafts(enabled:boolean){
 const [records,setRecords]=useState<SavedDraft[]>([])
 const [key,setKey]=useState<string>(()=>crypto.randomUUID())
 const [document,setDocumentState]=useState<DraftDocument>(()=>emptyDraft())
 const [saveError,setSaveError]=useState('')
 const memory=useRef(new Map<string,SavedDraft>())
 const revisions=useRef(new Map<string,number>())
 const archivedBranchIds=useRef(new Set<string>())
 const chain=useRef(Promise.resolve())
 const current=useRef({key,document});current.current={key,document}
 const context=useRef<{threadId:string|null;projectId:string;agentId:string;settings:ChatSettings}>({threadId:null,projectId:'',agentId:'',settings:{model:'',effort:'medium',mode:'default'}})
 const mounted=useRef(true)
 useEffect(()=>{mounted.current=true;return ()=>{mounted.current=false}},[])
 const refresh=useCallback(async()=>{if(!enabled)return false;try{const saved=await chat.listDrafts();if(Array.isArray(saved)&&mounted.current){const visible=saved.filter(d=>!(d.branchEdit&&archivedBranchIds.current.has(d.threadId??d.id)));setRecords(visible);setSaveError(current=>current.startsWith('Drafts are not saved:')?'':current);for(const d of visible){if(!memory.current.has(d.id))memory.current.set(d.id,d);revisions.current.set(d.id,Math.max(revisions.current.get(d.id)??0,d.revision))}}return Array.isArray(saved)}catch(reason){if(mounted.current)setSaveError(`Drafts are not saved: ${String(reason)}`);return false}},[enabled])
 const hideArchivedBranch=useCallback((threadId:string)=>{archivedBranchIds.current.add(threadId);setRecords(current=>current.filter(d=>!(d.branchEdit&&(d.threadId??d.id)===threadId)))},[])
 useEffect(()=>{
  if(!enabled)return
  let active=true
  let retries=0
  let timer:ReturnType<typeof setTimeout>|undefined
  const load=async()=>{
   const loaded=await refresh()
   if(loaded||!active||retries>=8)return
   const delay=Math.min(250*2**retries,5000)
   retries+=1
   timer=setTimeout(()=>{void load()},delay)
  }
  void load()
  return()=>{active=false;if(timer)clearTimeout(timer)}
 },[enabled,refresh])
 const persist=useCallback((id:string,doc:DraftDocument,contextOverride?:typeof context.current)=>{
  const ctx={...(contextOverride??context.current)};const record={id,threadId:ctx.threadId,projectId:ctx.projectId||null,agentId:ctx.agentId||null,document:doc,settings:ctx.settings}
  memory.current.set(id,{...record,revision:revisions.current.get(id)??0,updatedAt:new Date().toISOString()})
  if(!enabled)return
  chain.current=chain.current.then(async()=>{try{const result=await chat.saveDraft(record,revisions.current.get(id)??0);revisions.current.set(id,result.revision);if(mounted.current)setSaveError('');await refresh()}catch(reason){if(mounted.current)setSaveError(`Draft not saved: ${String(reason)}`)}})
 },[enabled,refresh])
 const setDocument=useCallback((next:DraftDocument|((previous:DraftDocument)=>DraftDocument))=>{const value=typeof next==='function'?next(current.current.document):next;current.current.document=value;setDocumentState(value);persist(current.current.key,value)},[persist])
 const switchDraft=useCallback(async(id:string,threadId:string|null,settings?:Partial<ChatSettings>)=>{
  current.current.key=id;setKey(id)
  const cached=memory.current.get(id)
  if(cached){setDocumentState(cached.document);current.current.document=cached.document;return cached}
  setDocumentState(emptyDraft());current.current.document=emptyDraft()
  if(!enabled)return undefined
  try{const saved=await chat.getDraft(id);if(current.current.key===id){memory.current.set(id,saved);revisions.current.set(id,saved.revision);setDocumentState(saved.document);current.current.document=saved.document;return saved}}catch{}
  context.current={...context.current,threadId,settings:{...context.current.settings,...settings}}
  return undefined
 },[enabled])
 const newDraft=useCallback(()=>{context.current={...context.current,threadId:null,agentId:''};const id=crypto.randomUUID();current.current={key:id,document:emptyDraft()};setKey(id);setDocumentState(emptyDraft());return id},[])
 const updateContext=useCallback((next:Partial<typeof context.current>)=>{
  context.current={...context.current,...next}
  persist(current.current.key,current.current.document)
 },[persist])
 const updateDraftDocument=useCallback((id:string,update:(previous:DraftDocument)=>DraftDocument,fallback:DraftDocument,contextSnapshot:typeof context.current)=>{
  const previous=memory.current.get(id)?.document??(current.current.key===id?current.current.document:fallback)
  persist(id,update(previous),contextSnapshot)
 },[persist])
 const promote=useCallback(async(threadId:string,originalDraftId:string,source?:{context:typeof context.current;document:DraftDocument;shouldAdopt:()=>boolean})=>{
  const id=originalDraftId
  if(!enabled)return
  await chain.current
  // An empty new chat may be created before the first draft write has completed.
  if(!memory.current.has(id)){
   const ctx=source?.context ?? context.current
   const doc=source?.document ?? (current.current.key===id?current.current.document:memory.current.get(id)?.document ?? emptyDraft())
   const saved=await chat.saveDraft({id,threadId:null,projectId:ctx.projectId||null,agentId:ctx.agentId||null,document:doc,settings:ctx.settings},0)
   memory.current.set(id,saved);revisions.current.set(id,saved.revision)
  }
  else if(source){
   const old=memory.current.get(id)!
   const ctx=source.context
   const saved=await chat.saveDraft({id,threadId:null,projectId:ctx.projectId||null,agentId:ctx.agentId||null,document:source.document,settings:ctx.settings},revisions.current.get(id)??old.revision)
   memory.current.set(id,saved);revisions.current.set(id,saved.revision)
  }
  const promoted=await chat.promoteDraft(id,threadId)
  memory.current.delete(id);memory.current.set(threadId,{...promoted,id:threadId,threadId});revisions.current.set(threadId,promoted.revision)
  if(current.current.key===id && (source?.shouldAdopt?.() ?? true)){current.current.key=threadId;context.current={...(source?.context ?? context.current),threadId};setKey(threadId)}
  await refresh()
 },[enabled,refresh])

 return {key,document,setDocument,records,saveError,context,switchDraft,newDraft,promote,updateContext,updateDraftDocument,refresh,hideArchivedBranch}
}
