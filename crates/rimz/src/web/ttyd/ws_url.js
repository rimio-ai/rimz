const rewriteWsUrl=url=>{
  let target=url;
  try{
    const parsed=new URL(url,window.location.href);
    if(parsed.host===window.location.host&&parsed.pathname.endsWith("/ws")){
      const search=new URLSearchParams(window.location.search);
      if(search.has("room")){
        parsed.search="";
        parsed.searchParams.set("arg",search.get("room"));
      }else parsed.search=window.location.search;
      target=parsed;
    }
  }catch(_){}
  return target;
};

const NativeWebSocket=window.WebSocket;
const installRoomWebSocketUrl=()=>{
  window.WebSocket=class extends NativeWebSocket{
    constructor(url,protocols){
      const target=rewriteWsUrl(url);
      if(protocols===undefined)super(target);
      else super(target,protocols);
    }
  };
};
