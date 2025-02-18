struct vs_in {
    float3 position_local : POS;
    float2 tex_pos : TEX;
};

Texture2D ObjTexture;
SamplerState ObjSamplerState;

struct vs_out {
    float4 position_clip : SV_POSITION;
    float2 tex_pos : TEXCOORD;
};

vs_out vs_main(vs_in input) {
    vs_out output = (vs_out)0;
    output.position_clip = float4(input.position_local, 1.0);
    output.tex_pos = float2(input.tex_pos.x, 1 - input.tex_pos.y);
    return output;
}

float4 ps_main(vs_out input) : SV_TARGET {
    return ObjTexture.Sample(ObjSamplerState, input.tex_pos);
}
