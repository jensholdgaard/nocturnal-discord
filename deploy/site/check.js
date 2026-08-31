// Renders every route of deploy/site/index.html under Node against site.json and data.json
// in the current directory, with a stubbed DOM, and prints what throws. Run before installing.
// usage: node deploy/site/check.js  (with site.json + data.json beside it, scp'd from the VM)

const fs=require('fs');
const site=JSON.parse(fs.readFileSync('site.json')), roster=JSON.parse(fs.readFileSync('data.json'));
const els={};
const mk=(id)=>({id, innerHTML:'', textContent:'', hidden:false, style:{}, addEventListener(){}, appendChild(){}, dataset:{}, offsetWidth:0, offsetHeight:0, toggleAttribute(){}, setAttribute(){}, removeAttribute(){}, getAttribute(){return null}, querySelectorAll(){return []}});
global.document={getElementById:(id)=>els[id]||(els[id]=mk(id)), createElement:()=>mk('x'), body:{appendChild(){}}, addEventListener(){}, querySelectorAll(){return []}, activeElement:null};
global.window={addEventListener(){}, scrollTo(){}, location:{hash:'#me'}};
global.location={hash:'#me'}; global.innerWidth=1200; global.innerHeight=800;
global.Intl=Intl;
global.fetch=async (url)=>{ if(url.startsWith('/data/site.json')) return {status:200, ok:true, json:async()=>site}; if(url.startsWith('/data/data.json')) return {ok:true, json:async()=>roster}; if(url.includes('whoami')) return {ok:true, json:async()=>({kind:'User',metadata:{name:'bisben_'}})}; if(url.startsWith('/prom')) return {ok:true, json:async()=>({data:{result:[]}})}; return {ok:false, status:404}; };
try { eval(require('fs').readFileSync(require('path').join(__dirname,'index.html'),'utf8').match(/<script>([\s\S]*)<\/script>/)[1]); } catch(e){ console.log('LOAD ERROR:', e.message); }
setTimeout(()=>{ for (const h of ['#raid','#me','#roster','#loot']) { location.hash=h; try{ render(); const html=els.main.innerHTML; console.log(h, 'OK', html.length, 'chars', html.includes('Could not load')?'(Could not load!)':''); }catch(e){ console.log(h, 'THROWS:', e.message, '\n   at', (e.stack||'').split('\n')[1]); } } }, 300);
