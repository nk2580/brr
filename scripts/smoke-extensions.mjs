// Exercise the real owned runtime using disposable plugin/MCP packages, never
// the user's Codex home, installed CLI, account, or existing connections.
import { spawn } from 'node:child_process'
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import readline from 'node:readline'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'

const root = mkdtempSync(path.join(tmpdir(), 'brr-extensions-'))
const home = path.join(root, 'engine')
const market = path.join(root, 'market')
const executable = path.resolve(process.argv[2] || 'src-tauri/binaries/brr-runtime')
const json = (file, value) => { mkdirSync(path.dirname(file), {recursive:true}); writeFileSync(file, JSON.stringify(value)) }
mkdirSync(home)
writeFileSync(path.join(home, 'config.toml'), '[features]\nremote_plugin=false\nplugin_discovery_prefetch=false\n')
const server = path.join(root, 'server.mjs')
writeFileSync(server, `import readline from 'node:readline';
const result = m => {
 switch(m.method) {
 case 'server/discover': if(!process.env.BRR_MODERN_SERVER)throw Error('Legacy fixture');return {resultType:'complete',supportedVersions:['2026-07-28'],capabilities:{tools:{},resources:{},prompts:{}},_meta:{'io.modelcontextprotocol/serverInfo':{name:'brr-modern-stdio-fixture',version:'1.0'}},ttlMs:0,cacheScope:'private'};
 case 'initialize': return {protocolVersion:'2025-11-25',capabilities:{tools:{},resources:{},prompts:{}},serverInfo:{name:'brr-fixture',version:'1.0'}};
 case 'ping': return {};
 case 'tools/list': return {tools:[{name:'echo',description:'Fixture echo',inputSchema:{type:'object',properties:{text:{type:'string'}}}}]};
 case 'resources/list': return {resources:[{uri:'fixture://message',name:'Fixture message',mimeType:'text/plain'}]};
 case 'resources/templates/list': return {resourceTemplates:[]};
 case 'resources/read': return {contents:[{uri:m.params.uri,mimeType:'text/plain',text:'resource verified'}]};
 case 'prompts/list': return {prompts:[{name:'greeting',description:'Fixture prompt',arguments:[{name:'city',required:true}]}]};
 case 'prompts/get': return {description:'Fixture prompt',messages:[{role:'user',content:{type:'text',text:'Hello '+m.params.arguments.city}}]};
 case 'completion/complete': return {completion:{values:['Brisbane'],total:1,hasMore:false}};
 case 'tools/call': return {content:[{type:'text',text:m.params.arguments.text}]};
 default: throw Error('Unknown method '+m.method);
 }
};
readline.createInterface({input:process.stdin}).on('line',line=>{const m=JSON.parse(line);if(m.id===undefined)return;try{const modern=!!process.env.BRR_MODERN_SERVER;if(modern&&!m.params?._meta?.['io.modelcontextprotocol/protocolVersion'])throw Error('Missing modern metadata');console.log(JSON.stringify({jsonrpc:'2.0',id:m.id,result:modern?{resultType:'complete',...result(m)}:result(m)}))}catch(e){console.log(JSON.stringify({jsonrpc:'2.0',id:m.id,error:{code:-32601,message:e.message}}))}});
`)
const mcp = {mcpServers:{fixture:{command:process.execPath,args:[server]}}}
for (const portable of [false,true]) {
  const name = portable ? 'portable-fixture' : 'codex-fixture'
  const plugin = path.join(market, 'plugins', name)
  const fixtureCommand = portable ? 'node' : process.execPath
  json(path.join(plugin, portable ? 'plugin.json' : '.codex-plugin/plugin.json'), portable ? {$schema:'https://agent-plugins.org/schemas/1.0.0/plugin.schema.json',name,version:'1.0.0',extensions:{'com.openai':{hooks:'./hooks/hooks.json'}}} : {name,version:'1.0.0',skills:'./skills',mcpServers:'./.mcp.json'})
  json(path.join(plugin, portable ? 'mcp.json' : '.mcp.json'), portable ? {$schema:'https://agent-plugins.org/schemas/1.0.0/mcp.schema.json',mcpServers:{
    fixture:{type:'stdio',command:fixtureCommand,args:[server],extensions:{'com.openai':{auth:{type:'none'}}}},
    needs_key:{type:'streamable-http',url:'https://example.invalid/mcp',extensions:{'com.openai':{auth:{type:'api_key',headerScheme:'bearer'}}}},
  }} : mcp)
  json(path.join(plugin,'hooks/hooks.json'), {hooks:{SessionStart:[{hooks:[{type:'command',command:'echo brr-fixture-hook'}]}]}})
  const skill = path.join(plugin,'skills','fixture-greeting','SKILL.md')
  mkdirSync(path.dirname(skill),{recursive:true})
  writeFileSync(skill,'---\nname: fixture-greeting\ndescription: Greet a fixture user.\n---\nSay hello to the user.\n')
}
json(path.join(market,'.agents/plugins/marketplace.json'), {name:'brr-smoke',plugins:['codex-fixture','portable-fixture'].map(name=>({name,source:{source:'local',path:'./plugins/'+name},policy:{installation:'AVAILABLE',authentication:'ON_USE'}}))})
const env = {...process.env, CODEX_HOME:home}
for (const key of ['OPENAI_API_KEY','CODEX_API_KEY','CODEX_AUTH_JSON','CODEX_ACCESS_TOKEN','OPENAI_BASE_URL']) delete env[key]
const child = spawn(executable, [], {cwd:home,env,stdio:['pipe','pipe','pipe']})
let id = 0
let stderr = ''
const pending = new Map()
let elicitationHandled = false
const modernRequests = []
const modern = createServer(async (request, response) => {
  if (request.method !== 'POST') { response.writeHead(405); response.end(); return }
  let body = ''
  for await (const data of request) body += data
  const message = JSON.parse(body)
  modernRequests.push(message)
  const send = result => { response.writeHead(200, {'content-type':'application/json'}); response.end(JSON.stringify({jsonrpc:'2.0',id:message.id,result})) }
  assert.equal(message.params?._meta?.['io.modelcontextprotocol/protocolVersion'], '2026-07-28')
  assert.ok(message.params?._meta?.['io.modelcontextprotocol/clientInfo']?.name)
  switch (message.method) {
    case 'server/discover': send({resultType:'complete',supportedVersions:['2026-07-28'],capabilities:{tools:{},resources:{},prompts:{},completions:{}},_meta:{'io.modelcontextprotocol/serverInfo':{name:'brr-modern-fixture',version:'1.0'}},ttlMs:0,cacheScope:'private'}); break
    case 'tools/list': send({resultType:'complete',tools:[]}); break
    case 'resources/list': send({resultType:'complete',resources:[{uri:'fixture://modern',name:'Modern fixture'}]}); break
    case 'resources/templates/list': send({resultType:'complete',resourceTemplates:[]}); break
    case 'resources/read': send({resultType:'complete',contents:[{uri:'fixture://modern',text:'modern resource verified'}]}); break
    case 'prompts/list': send({resultType:'complete',prompts:[{name:'modern-prompt'}]}); break
    case 'prompts/get':
      if (message.params.name === 'modern-mrtr') {
        assert.deepEqual(message.params.arguments, {city:'Brisbane'})
        if (!message.params.inputResponses) {
          send({resultType:'input_required',requestState:'opaque\n"brr-state"',inputRequests:{confirmation:{method:'elicitation/create',params:{mode:'form',message:'Confirm fixture prompt',requestedSchema:{type:'object',properties:{confirmed:{type:'boolean'}},required:['confirmed']}}}}})
        } else {
          assert.equal(message.params.requestState,'opaque\n"brr-state"')
          assert.equal(message.params.inputResponses.confirmation.action,'accept')
          assert.deepEqual(message.params.inputResponses.confirmation.content,{confirmed:true})
          send({resultType:'complete',messages:[{role:'user',content:{type:'text',text:'prompt MRTR verified'}}]})
        }
      } else send({resultType:'complete',messages:[{role:'user',content:{type:'text',text:'modern prompt verified'}}]})
      break
    case 'completion/complete': send({resultType:'complete',completion:{values:['modern'],total:1,hasMore:false}}); break
    default: response.writeHead(400); response.end(JSON.stringify({error:'Unexpected method '+message.method}))
  }
})
await new Promise(resolve => modern.listen(0, '127.0.0.1', resolve))
child.stderr.on('data', data => {stderr=(stderr+data.toString()).slice(-4000)})
child.on('error', error => {for(const request of pending.values())request.reject(error)})
child.on('exit', code => {for(const request of pending.values())request.reject(Error('Runtime exited '+code+': '+stderr))})
readline.createInterface({input:child.stdout}).on('line',line=>{
  let m;try{m=JSON.parse(line)}catch{return}
  if(m.method) {
    if(m.id!==undefined) {
      const expected=m.method==='mcpServer/elicitation/request'&&m.params?.message==='Confirm fixture prompt'
      if(expected)elicitationHandled=true
      child.stdin.write(JSON.stringify({id:m.id,result:expected?{action:'accept',content:{confirmed:true},_meta:null}:{action:'cancel',content:null,_meta:null}})+'\n')
    }
    return
  }
  const request=pending.get(m.id)
  if(request){clearTimeout(request.timer);pending.delete(m.id);m.error?request.reject(Error(m.error.message)):request.resolve(m.result)}
})
function rpc(method,params={}) {
  return new Promise((resolve,reject)=>{
    const current=++id
    const timer=setTimeout(()=>{pending.delete(current);reject(Error('Timed out: '+method))},60000)
    pending.set(current,{resolve,reject,timer})
    child.stdin.write(JSON.stringify({id:current,method,params})+'\n')
  })
}
try {
  await rpc('initialize',{clientInfo:{name:'brr_extensions_smoke',version:'0.1.0'},capabilities:{experimentalApi:true}})
  child.stdin.write('{"method":"initialized"}\n')
  await rpc('marketplace/add',{source:market})
  let catalog=await rpc('plugin/list',{marketplaceKinds:['local']})
  const source=catalog.marketplaces.find(item=>item.name==='brr-smoke')
  assert.ok(source,JSON.stringify(catalog))
  assert.equal(source.plugins.length,2)
  for(const plugin of source.plugins) {
    await rpc('plugin/install',{marketplacePath:source.path,pluginName:plugin.name})
    const detail=await rpc('plugin/read',{marketplacePath:source.path,pluginName:plugin.name})
    assert.equal(detail.plugin.summary.installed,true)
    assert.ok(detail.plugin.skills.length>0)
    const keyPath=`plugins.${JSON.stringify(detail.plugin.summary.id)}.enabled`
    await rpc('config/value/write',{keyPath,value:false,mergeStrategy:'replace'})
    const disabled=await rpc('plugin/read',{marketplacePath:source.path,pluginName:plugin.name})
    assert.equal(disabled.plugin.summary.enabled,false)
    await rpc('config/value/write',{keyPath,value:true,mergeStrategy:'replace'})
    const enabled=await rpc('plugin/read',{marketplacePath:source.path,pluginName:plugin.name})
    assert.equal(enabled.plugin.summary.enabled,true)
    if (plugin.name === 'portable-fixture') {
      assert.ok(enabled.plugin.mcpServerErrors?.some(error=>error.serverName==='needs_key' && error.authDescriptor?.type==='api_key' && /auth|key|unsupported/i.test(error.message)),JSON.stringify(enabled.plugin))
      assert.ok(enabled.plugin.mcpServers.includes('fixture'),JSON.stringify(enabled.plugin))
      assert.ok(!enabled.plugin.mcpServers.includes('needs_key'))
    }
  }
  const hookInventory=await rpc('hooks/list',{cwds:[home]})
  const hooks=hookInventory.data.flatMap(entry=>entry.hooks).filter(hook=>hook.pluginId?.includes('fixture'))
  await rpc('config/batchWrite',{edits:hooks.map(hook=>({keyPath:`hooks.state.${JSON.stringify(hook.key)}.trusted_hash`,value:hook.currentHash,mergeStrategy:'replace'})),reloadUserConfig:true})
  const trusted=await rpc('hooks/list',{cwds:[home]})
  assert.ok(trusted.data.flatMap(entry=>entry.hooks).filter(hook=>hook.pluginId?.includes('fixture')).every(hook=>hook.trustStatus==='trusted'))
  const statuses=await rpc('mcpServerStatus/list',{detail:'full'})
  const connection=statuses.data.find(item=>Object.values(item.tools).some(tool=>tool.name==='echo'))
  assert.ok(connection,JSON.stringify(statuses))
  const resource=await rpc('mcpServer/resource/read',{server:connection.name,uri:'fixture://message'})
  assert.match(JSON.stringify(resource),/resource verified/)
  const prompts=await rpc('mcpServer/prompts/list',{server:connection.name})
  assert.equal(prompts.prompts[0].name,'greeting')
  const prompt=await rpc('mcpServer/prompt/get',{server:connection.name,name:'greeting',arguments:{city:'Brisbane'}})
  assert.equal(prompt.messages[0].content.text,'Hello Brisbane')
  const completion=await rpc('mcpServer/completion/complete',{server:connection.name,ref:{type:'ref/prompt',name:'greeting'},argument:{name:'city',value:'Br'}})
  assert.deepEqual(completion.completion.values,['Brisbane'])
  await rpc('config/value/write',{keyPath:'mcp_servers.modern_fixture',value:{url:`http://127.0.0.1:${modern.address().port}/mcp`},mergeStrategy:'replace'})
  await rpc('config/mcpServer/reload')
  const modernStatus=await rpc('mcpServerStatus/list',{serverName:'modern_fixture',detail:'full'})
  assert.equal(modernStatus.data[0].serverInfo.name,'brr-modern-fixture')
  const modernResource=await rpc('mcpServer/resource/read',{server:'modern_fixture',uri:'fixture://modern'})
  assert.match(JSON.stringify(modernResource),/modern resource verified/)
  const modernPrompts=await rpc('mcpServer/prompts/list',{server:'modern_fixture'})
  assert.equal(modernPrompts.prompts[0].name,'modern-prompt')
  const modernPrompt=await rpc('mcpServer/prompt/get',{server:'modern_fixture',name:'modern-prompt'})
  assert.equal(modernPrompt.messages[0].content.text,'modern prompt verified')
  const modernCompletion=await rpc('mcpServer/completion/complete',{server:'modern_fixture',ref:{type:'ref/prompt',name:'modern-prompt'},argument:{name:'city',value:''}})
  assert.deepEqual(modernCompletion.completion.values,['modern'])
  const thread=await rpc('thread/start',{cwd:home,modelProvider:'openai',sandbox:'read-only',approvalPolicy:'on-request'})
  const mrtr=await rpc('mcpServer/prompt/get',{server:'modern_fixture',threadId:thread.thread.id,name:'modern-mrtr',arguments:{city:'Brisbane'}})
  assert.equal(mrtr.messages[0].content.text,'prompt MRTR verified')
  assert.equal(elicitationHandled,true)
  assert.ok(modernRequests.every(request=>request.params._meta['io.modelcontextprotocol/clientCapabilities']))
  await rpc('config/value/write',{keyPath:'mcp_servers.modern_stdio',value:{command:process.execPath,args:[server],env:{BRR_MODERN_SERVER:'1'}},mergeStrategy:'replace'})
  await rpc('config/mcpServer/reload')
  const stdioStatus=await rpc('mcpServerStatus/list',{serverName:'modern_stdio',detail:'full'})
  assert.equal(stdioStatus.data[0].serverInfo.name,'brr-modern-stdio-fixture')
  const stdioPrompt=await rpc('mcpServer/prompt/get',{server:'modern_stdio',name:'greeting',arguments:{city:'Brisbane'}})
  assert.equal(stdioPrompt.messages[0].content.text,'Hello Brisbane')
  console.log('Protocol phase passed: stdio/HTTP discovery, resources, prompts, completion and threaded elicitation continuation.')
  assert.equal(hooks.length,2,JSON.stringify(hookInventory))
  catalog=await rpc('plugin/list',{marketplaceKinds:['local']})
  for(const plugin of catalog.marketplaces.find(item=>item.name==='brr-smoke').plugins) await rpc('plugin/uninstall',{pluginId:plugin.id})
  const final=await rpc('plugin/list',{marketplaceKinds:['local']})
  assert.ok(final.marketplaces.find(item=>item.name==='brr-smoke').plugins.every(plugin=>!plugin.installed))
  console.log('Extensions smoke passed: real Codex + portable package install/read/toggle/hooks/uninstall; legacy + modern stdio and MCP 2026 HTTP; resources, prompts, completion and elicitation continuation with opaque state.')
} finally {
  for(const request of pending.values())clearTimeout(request.timer)
  const closed = new Promise(resolve=>child.once('close',resolve))
  child.stdin.end()
  const killer=setTimeout(()=>child.kill('SIGKILL'),5000)
  await closed
  clearTimeout(killer)
  await new Promise(resolve => modern.close(resolve))
  rmSync(root,{recursive:true,force:true})
}
