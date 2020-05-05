pub mod pb {
    tonic::include_proto!("grpc.examples.echo");
}

use pb::{echo_client::EchoClient, EchoRequest};
use tonic::transport::Channel;
use std::collections::VecDeque;
use tonic::{transport::Endpoint};
use tonic::{transport::EndpointManager};
use std::sync::Arc;
use tokio::sync::Mutex;
use std::sync::atomic::{AtomicBool,Ordering::SeqCst};
use tokio::time::timeout;



#[derive(Clone)]
struct SimpleEndpointManager{
    to_add: Arc<Mutex<VecDeque<(usize, Endpoint)>>>,
    to_remove: Arc<Mutex<VecDeque<usize>>>,

}


impl SimpleEndpointManager{
    fn new()->Self{
	SimpleEndpointManager{
	    to_add: Arc::new(Mutex::new(VecDeque::new())),
	    to_remove: Arc::new(Mutex::new(VecDeque::new())),
	}
    }
    async fn add(&self,key:usize, endpoint:Endpoint)->usize{
	let mut to_add = self.to_add.lock().await;
	to_add.push_back((key,endpoint));
	key
    }
    
    async fn remove(&self, key:usize){
	let mut to_remove = self.to_remove.lock().await;
	to_remove.push_back(key);	
    }
}


impl EndpointManager for SimpleEndpointManager{
    fn to_add(&self)->Option<(usize,Endpoint)>{
	match self.to_add.try_lock(){
	    Ok(mut to_add) => to_add.pop_front(),
	    Err(e) => {
		println!("error {:?}",e);
		None
	    }
	}
		
    }

    fn to_remove(& self)->Option<usize>{
	match self.to_remove.try_lock(){
	    Ok(mut to_remove) => to_remove.pop_front(),
	    Err(_) => None
	}

    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {    
    let e1 = Endpoint::from_static("http://[::1]:50051").timeout(std::time::Duration::from_secs(1));
    let e2 = Endpoint::from_static("http://[::1]:50052").timeout(std::time::Duration::from_secs(1));
    let em = Box::new(SimpleEndpointManager::new());
    let channel = Channel::balance_with_manager(em.clone());
    let mut client = EchoClient::new(channel);

    let done= Arc::new(AtomicBool::new(false));
    let demo_done = done.clone();
    tokio::spawn(async move{	
	tokio::time::delay_for(tokio::time::Duration::from_secs(5)).await;
	println!("Added first endpoint");
	let e1_id = em.add(1,e1).await;
	tokio::time::delay_for(tokio::time::Duration::from_secs(5)).await;
	println!("Added second endpoint");
    	let e2_id = em.add(2,e2).await;
	tokio::time::delay_for(tokio::time::Duration::from_secs(5)).await;
	println!("Removed first endpoint");
	em.remove(e1_id).await;
	tokio::time::delay_for(tokio::time::Duration::from_secs(5)).await;
	println!("Removed second endpoint");
	em.remove(e2_id).await;
	tokio::time::delay_for(tokio::time::Duration::from_secs(5)).await;
	println!("Added third endpoint");
	let e3 = Endpoint::from_static("http://[::1]:50051");
	let e3_id = em.add(3,e3).await;
	tokio::time::delay_for(tokio::time::Duration::from_secs(5)).await;
	println!("Removed third endpoint");
	em.remove(e3_id).await;
	demo_done.swap(true,SeqCst);
    });
    
    while !done.load(SeqCst){
	tokio::time::delay_for(tokio::time::Duration::from_millis(500)).await;
        let request = tonic::Request::new(EchoRequest {
            message: "hello".into(),
        });

	let rx = client.unary_echo(request);
	if let Ok(resp) = timeout(tokio::time::Duration::from_secs(10), rx).await {
	    println!("RESPONSE={:?}", resp);
	}else{
	    println!("did not receive value within 10 secs");
	}	
    }

    println!("... Bye");

    Ok(())
}


