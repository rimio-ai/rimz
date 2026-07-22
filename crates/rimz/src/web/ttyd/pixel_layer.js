const installPixelLayer=term=>{
  const screen=term.element.querySelector(".xterm-screen");
  if(!screen||typeof createImageBitmap!=="function")return;
  const ESC=0x1b;
  const MAX_APC_BYTES=8*1024*1024;
  const MAX_IMAGE_BYTES=4*1024*1024;
  const MAX_IMAGES=128;
  const UTF8_MASKS=[0,0x7f,0x1f,0x0f,0x07];
  const UTF8_MINIMUMS=[0,0,0x80,0x800,0x10000];
  const encoder=new TextEncoder();
  const decoder=new TextDecoder();
  const placeholderBytes=encoder.encode(String.fromCodePoint(RIMZ_PIXEL_PLACEHOLDER));
  const hideGlyph=encoder.encode("\x1b[8m");
  const showGlyph=encoder.encode("\x1b[28m");
  const diacriticIndexes=new Map(RIMZ_PIXEL_DIACRITICS.map((value,index)=>[value.codePointAt(0),index]));
  const images=new Map();
  const placements=new Map();
  const pendingDecodes=new Map();
  let transfer=null;
  let decodeToken=0;
  let carry=new Uint8Array();
  let dropping=false;
  let dropSawEscape=false;
  let placeholderCarry=new Uint8Array();

  const canvas=document.createElement("canvas");
  canvas.className="rimz-pixel-layer";
  canvas.dataset.rimzPixelProtocol=String(RIMZ_PIXEL_PROTOCOL);
  canvas.style.cssText="position:absolute;inset:0;pointer-events:none;z-index:10";
  if(getComputedStyle(screen).position==="static")screen.style.position="relative";
  screen.append(canvas);
  const context=canvas.getContext("2d");
  if(!context){canvas.remove();return;}
  context.imageSmoothingEnabled=false;
  let canvasBlank=true;

  const closeBitmap=bitmap=>{try{bitmap.close();}catch(_){}};
  const clearCanvas=()=>{
    const dpr=window.devicePixelRatio||1;
    context.setTransform(dpr,0,0,dpr,0,0);
    context.clearRect(0,0,canvas.width/dpr,canvas.height/dpr);
    canvasBlank=true;
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
  const utf8Width=byte=>byte<0x80?1:byte<0xe0?2:byte<0xf0?3:4;
  const readUtf8=(input,offset)=>{
    const width=utf8Width(input[offset]);
    const end=offset+width;
    if(end>input.length)return null;
    let value=input[offset]&UTF8_MASKS[width];
    for(let index=offset+1;index<end;index++){
      const byte=input[index];
      if((byte&0xc0)!==0x80)return {end,value:-1};
      value=(value<<6)|(byte&0x3f);
    }
    if(value<UTF8_MINIMUMS[width]||value>0x10ffff||(value>=0xd800&&value<=0xdfff))value=-1;
    return {end,value};
  };
  // xterm keeps the Kitty image id in each placeholder cell's foreground, but
  // its WebGL renderer paints a colored fallback glyph for U+10EEEE. Mark the
  // complete base + row + column cluster invisible before xterm parses it.
  // Keeping one SGR state across the three codepoints lets xterm combine them
  // into one cell; CSI 28 then restores following text without changing color.
  const suppressPlaceholderGlyphs=chunk=>{
    const input=placeholderCarry.length?joinBytes([placeholderCarry,chunk]):chunk;
    placeholderCarry=new Uint8Array();
    const output=[];
    let literalStart=0;
    let offset=0;
    let concealed=false;
    const restore=()=>{
      if(!concealed)return;
      output.push(showGlyph);
      concealed=false;
    };
    const pushLiteral=(start,end)=>{
      if(start>=end)return;
      restore();
      output.push(input.subarray(start,end));
    };
    while(offset<input.length){
      if(input[offset]!==placeholderBytes[0]){offset++;continue;}
      let matched=1;
      while(matched<placeholderBytes.length&&offset+matched<input.length&&input[offset+matched]===placeholderBytes[matched])matched++;
      if(matched===placeholderBytes.length){
        let clusterEnd=offset+placeholderBytes.length;
        let valid=true;
        for(let mark=0;mark<2;mark++){
          if(clusterEnd>=input.length){clusterEnd=-1;break;}
          const decoded=readUtf8(input,clusterEnd);
          if(!decoded){clusterEnd=-1;break;}
          if(!diacriticIndexes.has(decoded.value)){valid=false;break;}
          clusterEnd=decoded.end;
        }
        if(clusterEnd<0){
          pushLiteral(literalStart,offset);
          placeholderCarry=input.slice(offset);
          literalStart=input.length;
          break;
        }
        if(!valid){offset+=placeholderBytes.length;continue;}
        pushLiteral(literalStart,offset);
        if(!concealed){output.push(hideGlyph);concealed=true;}
        output.push(input.subarray(offset,clusterEnd));
        offset=clusterEnd;
        literalStart=offset;
        continue;
      }
      if(offset+matched===input.length){
        pushLiteral(literalStart,offset);
        placeholderCarry=input.slice(offset);
        literalStart=input.length;
        break;
      }
      offset++;
    }
    pushLiteral(literalStart,input.length);
    restore();
    return joinBytes(output);
  };

  const cellSize=rect=>({
    width:rect.width/Math.max(1,term.cols),
    height:rect.height/Math.max(1,term.rows),
  });
  const fittedImageRect=(image,placement,size,x,y)=>{
    const boxWidth=placement.cols*size.width;
    const boxHeight=placement.rows*size.height;
    const scale=Math.min(boxWidth/image.width,boxHeight/image.height);
    const width=image.width*scale;
    const height=image.height*scale;
    return {
      x:x+(boxWidth-width)/2,
      y:y+boxHeight-height,
      width,
      height,
    };
  };
  const draw=()=>{
    if(placements.size===0&&images.size===0&&canvasBlank)return;
    const rect=resizeCanvas();
    context.clearRect(0,0,rect.width,rect.height);
    canvasBlank=true;
    const size=cellSize(rect);
    const buffer=term.buffer.active;
    const viewport=buffer.viewportY;
    const groups=new Map();
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
        const id=cell.getFgColor()&0xffffff;
        const placement=placements.get(id);
        if(!placement||sourceRow===undefined||sourceCol===undefined||sourceRow>=placement.rows||sourceCol>=placement.cols)continue;
        const x=col*size.width;
        const y=row*size.height;
        const originCol=col-sourceCol;
        const originRow=row-sourceRow;
        const key=`${id}:${originCol}:${originRow}`;
        let group=groups.get(key);
        if(!group){
          group={image:images.get(id),placement,x:originCol*size.width,y:originRow*size.height,cells:[]};
          groups.set(key,group);
        }
        group.cells.push({x,y});
      }
    }
    const dpr=window.devicePixelRatio||1;
    const snap=value=>Math.round(value*dpr)/dpr;
    for(const {image,placement,x,y,cells} of groups.values()){
      if(!image)continue;
      context.save();
      context.beginPath();
      for(const cell of cells){
        const left=snap(cell.x);
        const top=snap(cell.y);
        const right=snap(cell.x+size.width);
        const bottom=snap(cell.y+size.height);
        context.rect(left,top,right-left,bottom-top);
      }
      context.clip();
      if(placement.rows>1){
        const fitted=fittedImageRect(image,placement,size,x,y);
        context.drawImage(image,fitted.x,fitted.y,fitted.width,fitted.height);
      }else{
        context.drawImage(image,x,y,placement.cols*size.width,size.height);
      }
      context.restore();
      canvasBlank=false;
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
    const forwarded=suppressPlaceholderGlyphs(scan(bytes));
    if(forwarded.length)return write(forwarded,callback);
    if(typeof callback==="function")queueMicrotask(callback);
  };
  term.onRender(draw);
  term.onScroll(draw);
  term.onResize(()=>{clearCanvas();draw();});
  new ResizeObserver(()=>{clearCanvas();draw();}).observe(screen);
  const reset=term.reset.bind(term);
  term.reset=(...args)=>{clearImages();const result=reset(...args);scheduleDraw();return result;};
  scheduleDraw();
};
