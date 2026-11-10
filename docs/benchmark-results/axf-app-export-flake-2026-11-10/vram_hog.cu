#include <cstdio>
#include <cstdlib>
#include <unistd.h>
#include <cuda_runtime.h>
// hog MB_TO_LEAVE: allocate all free VRAM except MB_TO_LEAVE MiB, then hold until killed
int main(int c, char**v){ size_t leave=(size_t)atol(v[1])<<20; size_t f,t; cudaMemGetInfo(&f,&t);
 if(f<=leave){puts("nothing to take");return 1;} void*p; if(cudaMalloc(&p,f-leave)){puts("fail");return 1;}
 printf("held %zu MiB, free was %zu MiB\n",(f-leave)>>20,f>>20); fflush(stdout); pause(); }
