const installPixelLayer=term=>{
  const screen=term.element.querySelector(".xterm-screen");
  if(!screen||typeof createImageBitmap!=="function")return;
  const ESC=0x1b;
  const MAX_APC_BYTES=8*1024*1024;
  const MAX_IMAGE_BYTES=4*1024*1024;
  const MAX_IMAGES=128;
  const encoder=new TextEncoder();
  const decoder=new TextDecoder();
  const diacriticIndexes=new Map(RIMZ_PIXEL_DIACRITICS.map((value,index)=>[value.codePointAt(0),index]));
  const images=new Map();
  const placements=new Map();
  const pendingDecodes=new Map();
  let transfer=null;
  let decodeToken=0;
  let carry=new Uint8Array();
  let dropping=false;
  let dropSawEscape=false;

  const canvas=document.createElement("canvas");
  canvas.className="rimz-pixel-layer";
  canvas.dataset.rimzPixelProtocol=String(RIMZ_PIXEL_PROTOCOL);
  canvas.style.cssText="position:absolute;inset:0;pointer-events:none;z-index:10";
  if(getComputedStyle(screen).position==="static")screen.style.position="relative";
  screen.append(canvas);
  const context=canvas.getContext("2d");
  if(!context){canvas.remove();return;}
  context.imageSmoothingEnabled=false;

  const closeBitmap=bitmap=>{try{bitmap.close();}catch(_){}};
  const clearCanvas=()=>{
    const dpr=window.devicePixelRatio||1;
    context.setTransform(dpr,0,0,dpr,0,0);
    context.clearRect(0,0,canvas.width/dpr,canvas.height/dpr);
  };
  const resizeCanvas=()=>{
    const rect=screen.getBoundingClientRect();
    const dpr=window.devicePixelRatio||1;
    const width=Math.max(1,Math.round(rect.width*dpr));
    const height=Math.max(1,Math.round(rect.height*dpr));
    if(canvas.width!==width||canvas.height!==height){
      canvas.width=width;
      canvas.height=height;
      canvas.style.width=`${rect.width}px`;
      canvas.style.height=`${rect.height}px`;
    }
    context.setTransform(dpr,0,0,dpr,0,0);
    context.imageSmoothingEnabled=false;
    return rect;
  };
  const deleteImage=id=>{
    pendingDecodes.delete(id);
    const image=images.get(id);
    if(image)closeBitmap(image);
    images.delete(id);
    placements.delete(id);
  };
  const clearImages=()=>{
    pendingDecodes.clear();
    for(const image of images.values())closeBitmap(image);
    images.clear();
    placements.clear();
    transfer=null;
    clearCanvas();
  };
  const evictOldest=(map,onEvict)=>{
    while(map.size>=MAX_IMAGES){
      const oldest=map.keys().next().value;
      const value=map.get(oldest);
      map.delete(oldest);
      onEvict(oldest,value);
    }
  };
  const retainImage=(id,bitmap)=>{
    const previous=images.get(id);
    if(previous){
      images.delete(id);
      closeBitmap(previous);
    }else{
      evictOldest(images,(_id,image)=>closeBitmap(image));
    }
    images.set(id,bitmap);
    scheduleDraw();
  };
  const decodeImage=(id,payload)=>{
    if(pendingDecodes.has(id)||pendingDecodes.size>=MAX_IMAGES)return;
    let binary;
    try{binary=atob(payload);}catch(_){return;}
    const bytes=new Uint8Array(binary.length);
    for(let index=0;index<binary.length;index++)bytes[index]=binary.charCodeAt(index);
    const token=++decodeToken;
    pendingDecodes.set(id,token);
    createImageBitmap(new Blob([bytes],{type:"image/png"})).then(bitmap=>{
      if(pendingDecodes.get(id)!==token){closeBitmap(bitmap);return;}
      pendingDecodes.delete(id);
      retainImage(id,bitmap);
    }).catch(()=>{
      if(pendingDecodes.get(id)===token)pendingDecodes.delete(id);
    });
  };
  const integer=value=>typeof value==="string"&&/^\d+$/.test(value)?Number(value):null;
  const parseControl=value=>{
    const fields={};
    for(const part of value.split(",")){
      const split=part.indexOf("=");
      if(split>0)fields[part.slice(0,split)]=part.slice(split+1);
    }
    return fields;
  };
  const rememberPlacement=(id,cols,rows)=>{
    if(!placements.has(id))evictOldest(placements,()=>{});
    else placements.delete(id);
    placements.set(id,{cols,rows});
    scheduleDraw();
  };
  const handleApc=bytes=>{
    const separator=bytes.indexOf(0x3b);
    if(separator<0)return;
    const fields=parseControl(decoder.decode(bytes.subarray(0,separator)));
    const payload=decoder.decode(bytes.subarray(separator+1));
    if(fields.a==="t"){
      transfer=null;
      const id=integer(fields.i);
      if(fields.f!=="100"||id===null||id<1||id>0xffffff||payload.length>MAX_IMAGE_BYTES)return;
      transfer={id,chunks:[payload],size:payload.length};
      if(fields.m!=="1"){
        decodeImage(id,payload);
        transfer=null;
      }
      return;
    }
    if(fields.a===undefined&&transfer){
      if(transfer.size+payload.length>MAX_IMAGE_BYTES){transfer=null;return;}
      transfer.chunks.push(payload);
      transfer.size+=payload.length;
      if(fields.m!=="1"){
        decodeImage(transfer.id,transfer.chunks.join(""));
        transfer=null;
      }
      return;
    }
    transfer=null;
    if(fields.a==="p"&&fields.U==="1"){
      const id=integer(fields.i);
      const cols=integer(fields.c);
      const rows=integer(fields.r);
      if(id!==null&&id>0&&id<=0xffffff&&cols!==null&&cols>0&&cols<=RIMZ_PIXEL_DIACRITICS.length&&rows!==null&&rows>0&&rows<=RIMZ_PIXEL_DIACRITICS.length){
        rememberPlacement(id,cols,rows);
      }
    }else if(fields.a==="d"&&fields.d==="i"){
      const id=integer(fields.i);
      if(id!==null)deleteImage(id);
      scheduleDraw();
    }
  };
  const joinBytes=parts=>{
    const length=parts.reduce((sum,part)=>sum+part.length,0);
    const joined=new Uint8Array(length);
    let offset=0;
    for(const part of parts){joined.set(part,offset);offset+=part.length;}
    return joined;
  };
  const scan=chunk=>{
    let input=carry.length?joinBytes([carry,chunk]):chunk;
    carry=new Uint8Array();
    let offset=0;
    if(dropping){
      if(dropSawEscape&&input[0]===0x5c){
        dropping=false;
        dropSawEscape=false;
        offset=1;
      }else{
        let end=-1;
        for(let index=0;index+1<input.length;index++)if(input[index]===ESC&&input[index+1]===0x5c){end=index;break;}
        if(end<0){dropSawEscape=input[input.length-1]===ESC;return new Uint8Array();}
        dropping=false;
        dropSawEscape=false;
        offset=end+2;
      }
    }
    const output=[];
    while(offset<input.length){
      let start=-1;
      for(let index=offset;index+2<input.length;index++){
        if(input[index]===ESC&&input[index+1]===0x5f&&input[index+2]===0x47){start=index;break;}
      }
      if(start<0){
        let keep=0;
        if(input[input.length-1]===ESC)keep=1;
        else if(input.length>=2&&input[input.length-2]===ESC&&input[input.length-1]===0x5f)keep=2;
        if(input.length-keep>offset)output.push(input.subarray(offset,input.length-keep));
        if(keep)carry=input.slice(input.length-keep);
        break;
      }
      if(start>offset)output.push(input.subarray(offset,start));
      let end=-1;
      for(let index=start+3;index+1<input.length;index++)if(input[index]===ESC&&input[index+1]===0x5c){end=index;break;}
      if(end<0){
        carry=input.slice(start);
        if(carry.length>MAX_APC_BYTES){carry=new Uint8Array();dropping=true;dropSawEscape=false;}
        break;
      }
      handleApc(input.subarray(start+3,end));
      offset=end+2;
    }
    return joinBytes(output);
  };

  const cellSize=rect=>{
    const dimensions=term._core&&term._core._renderService&&term._core._renderService.dimensions;
    const cell=dimensions&&dimensions.css&&dimensions.css.cell;
    return {
      width:cell&&cell.width||rect.width/Math.max(1,term.cols),
      height:cell&&cell.height||rect.height/Math.max(1,term.rows),
    };
  };
  const draw=()=>{
    const rect=resizeCanvas();
    context.clearRect(0,0,rect.width,rect.height);
    const size=cellSize(rect);
    const buffer=term.buffer.active;
    const viewport=buffer.viewportY;
    const theme=term.options.theme||{};
    const viewportElement=term.element.querySelector(".xterm-viewport");
    const background=theme.background||(viewportElement&&getComputedStyle(viewportElement).backgroundColor)||"#000";
    for(let row=0;row<term.rows;row++){
      const line=buffer.getLine(viewport+row);
      if(!line)continue;
      for(let col=0;col<Math.min(term.cols,line.length);col++){
        const cell=line.getCell(col);
        if(!cell)continue;
        const chars=Array.from(cell.getChars());
        if(chars.length<3||chars[0].codePointAt(0)!==RIMZ_PIXEL_PLACEHOLDER)continue;
        const sourceRow=diacriticIndexes.get(chars[1].codePointAt(0));
        const sourceCol=diacriticIndexes.get(chars[2].codePointAt(0));
        const x=col*size.width;
        const y=row*size.height;
        context.fillStyle=background;
        context.fillRect(x,y,size.width,size.height);
        const id=cell.getFgColor()&0xffffff;
        const placement=placements.get(id);
        const image=images.get(id);
        if(!placement||!image||sourceRow===undefined||sourceCol===undefined||sourceRow>=placement.rows||sourceCol>=placement.cols)continue;
        const sourceWidth=image.width/placement.cols;
        const sourceHeight=image.height/placement.rows;
        context.drawImage(image,sourceCol*sourceWidth,sourceRow*sourceHeight,sourceWidth,sourceHeight,x,y,size.width,size.height);
      }
    }
  };
  let drawQueued=false;
  function scheduleDraw(){
    if(drawQueued)return;
    drawQueued=true;
    window.requestAnimationFrame(()=>{drawQueued=false;draw();});
  }

  const write=term.write.bind(term);
  term.write=(data,callback)=>{
    const bytes=typeof data==="string"?encoder.encode(data):data;
    if(!(bytes instanceof Uint8Array))return write(data,callback);
    const forwarded=scan(bytes);
    if(forwarded.length)return write(forwarded,callback);
    if(typeof callback==="function")queueMicrotask(callback);
  };
  term.onRender(scheduleDraw);
  term.onScroll(scheduleDraw);
  term.onResize(()=>{clearCanvas();scheduleDraw();});
  new ResizeObserver(()=>{clearCanvas();scheduleDraw();}).observe(screen);
  const reset=term.reset.bind(term);
  term.reset=(...args)=>{clearImages();const result=reset(...args);scheduleDraw();return result;};
  scheduleDraw();
};
